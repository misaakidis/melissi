----------------------------- MODULE Neighbourhood -----------------------------
(* Neighbourhood construction — the discovery LOGIC, companion to PullSyncerE in
   the IntervalSettlement mould: a minimal spec isolating the one emergent
   property the kademlia layer must hold, so the Rust `neighbourhood` crate can
   refine it the way `machine` refines the scheduling core.

   THE SETTING. A node sits at an overlay; every other peer falls in a proximity
   bin 0..MaxBin by shared-leading-bits with that overlay (bin MaxBin = closest).
   Discovery (the hive push, verified separately) supplies a fixed pool of
   reachable CANDIDATES per bin — `Cand[b]`. This module is the policy that turns
   that pool into a CONNECTED topology: how many peers to hold per bin, and which
   bins to fill densely. It abstracts the wire entirely; `conn[b]` is just a count.

   THE THEORY (bee `pkg/topology/kademlia`, spec §2.2.1). A bin is SATURATED at K
   connected peers (bee `SaturationPeers`; spec NHOOD_PEER_COUNT). DEPTH is the
   shallowest unsaturated bin — capped at MaxBin, since the closest bin is always
   part of the neighbourhood. The NEIGHBOURHOOD is the bins at or beyond depth:
   the slice of the address space the node is responsible for, where it must be
   connected to EVERY reachable peer (redundancy, and the reserve it serves over
   pull-sync). Shallower bins need only K — enough to route outward.

   So the optimal end-state is: every neighbourhood bin fully connected
   (`conn[b] = Cand[b]` for `b >= Depth`), every shallower bin trimmed to exactly
   K. As shallow bins saturate, depth rises, and a bin that was deep (densely
   filled) becomes shallow — its surplus must be SHED, or the working set grows
   without bound. That shedding is the prune (bee's HighWaterMark).

   KNOBS (each a design choice, each ablated):
     PrioritizeNbhd -- fill neighbourhood bins past K, to every candidate. OFF =
                       a flat "K per bin everywhere" policy: the neighbourhood is
                       never densely connected, so the node cannot redundantly
                       store/serve its slice -> NeighbourhoodComplete fails
                       (MC_nhood_flat). The deep bins stall at K < Cand[b].
     Pruning        -- shed a shallow bin's surplus once depth rises past it. OFF
                       -> a formerly-deep bin keeps its dense fill forever, below
                       depth: the working set never contracts -> Bounded fails
                       (MC_nhood_noprune). Saturation and neighbourhood density
                       still hold — the choice is about resource bound, not reach.

   COMPOSITION. Discovery (hive) proves the candidate pool is authentic (signed
   bindings). This module proves the policy converges to the optimal topology
   given that pool. Together: the node ends connected to exactly the right peers.

   OUT OF SCOPE, by design (documented, not modelled): the LowWaterMark depth edge
   (too few peers -> depth 0); reachability churn (peers dropping is the dual of
   Cand shrinking — a separate liveness story); and the overlay arithmetic itself
   (proximity is `overlay::proximity`, pinned by vector, not re-derived here). *)
EXTENDS Naturals, FiniteSets

CONSTANTS
  MaxBin,          \* bins are 0..MaxBin ; MaxBin is the closest (always neighbourhood)
  Cand,            \* [0..MaxBin -> Nat] : reachable candidate peers per bin
  K,               \* saturation target per bin (NHOOD_PEER_COUNT / SaturationPeers)
  PrioritizeNbhd,  \* BOOLEAN : fill neighbourhood bins past K (the density rule)
  Pruning          \* BOOLEAN : shed a shallow bin's surplus once depth passes it

Bins == 0..MaxBin

ASSUME MaxBin \in Nat /\ K \in Nat /\ K > 0
ASSUME Cand \in [Bins -> Nat]
ASSUME PrioritizeNbhd \in BOOLEAN /\ Pruning \in BOOLEAN

VARIABLES
  conn       \* [Bins -> Nat] : peers currently connected in each bin

vars == <<conn>>

Saturated(b) == conn[b] >= K

\* Depth: the shallowest unsaturated bin, capped at MaxBin (the closest bin is
\* always neighbourhood — bee never prunes it). All saturated -> MaxBin.
Unsaturated == { b \in Bins : ~Saturated(b) }
Depth ==
  IF Unsaturated = {} THEN MaxBin
  ELSE CHOOSE d \in Unsaturated : \A x \in Unsaturated : d <= x

InNeighbourhood(b) == b >= Depth

TypeOK ==
  /\ conn \in [Bins -> Nat]
  /\ \A b \in Bins : conn[b] <= Cand[b]

Init == conn = [b \in Bins |-> 0]

\* Connect one more candidate in bin b. A bin still draws connections while it is
\* under K (route-saturation, every bin) OR it is in the neighbourhood and the
\* density rule is on (connect to every close peer).
Fill(b) ==
  /\ conn[b] < Cand[b]
  /\ (conn[b] < K \/ (PrioritizeNbhd /\ InNeighbourhood(b)))
  /\ conn' = [conn EXCEPT ![b] = @ + 1]

\* Shed surplus from a bin that has fallen below depth: it was filled densely as a
\* neighbourhood bin, depth has since risen past it, so trim back toward K.
Trim(b) ==
  /\ Pruning
  /\ ~InNeighbourhood(b)
  /\ conn[b] > K
  /\ conn' = [conn EXCEPT ![b] = @ - 1]

Next == \E b \in Bins : Fill(b) \/ Trim(b)

\* The node's own steps are obligated: it keeps dialling candidates and shedding
\* surplus until the topology settles. Nothing here is adversarial-may.
Fairness ==
  /\ \A b \in Bins : WF_vars(Fill(b))
  /\ \A b \in Bins : WF_vars(Trim(b))

Spec == Init /\ [][Next]_vars /\ Fairness

-----------------------------------------------------------------------------
\* --- safety ---
\* never claim more peers than exist in a bin (a connection needs a candidate).
ConnLeCand == \A b \in Bins : conn[b] <= Cand[b]

\* --- liveness ---
\* every bin reaches as many peers as it can, up to K — the node is well-routed.
Saturates ==
  <>[](\A b \in Bins : conn[b] >= IF Cand[b] < K THEN Cand[b] ELSE K)

\* the neighbourhood is DENSE: every bin at/beyond depth holds every reachable
\* peer. This is the property the reserve/serving depends on, and the one the
\* flat "K everywhere" policy cannot reach.
NeighbourhoodComplete ==
  <>[](\A b \in Bins : InNeighbourhood(b) => conn[b] = Cand[b])

\* the working set stays bounded: a bin below depth holds at most K. As depth
\* rises, formerly-dense bins must be shed back — without the prune they aren't.
Bounded ==
  <>[](\A b \in Bins : ~InNeighbourhood(b) => conn[b] <= K)
=============================================================================
