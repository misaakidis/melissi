------------------------------ MODULE WindowedLoad ------------------------------
(* Windowed load — the serve-balance half of "no barrier, windowed", companion to
   DiscoveryBarrier (the liveness half: no wedge). This module REFINES the shipped
   scheduler (melissi `Node::round`, node/src/lib.rs:394-444) — not an idealisation
   of it. Two things that distinguishes it from a discovery barrier:

     - PER-CHUNK least-loaded. The scheduler routes each chunk to the least-loaded
       holder it currently knows (node.rs:420-436). The puller wants the whole
       offered page; balance is this routing rule over the holders who have offered
       a given chunk, NOT a per-peer commit cap. Modelled by Assign giving +1 to a
       least-loaded ARRIVED holder.
     - NO WAIT. `round()` fires as each offer lands and routes over the holders
       known SO FAR — there is no barrier. So the choice set is assembled
       incrementally, and a holder that offers late starts BEHIND. Modelled by
       letting Offer and Assign interleave freely.

   THE GUARANTEE this buys, and its limit. Two properties hold UNCONDITIONALLY and
   refine the code directly:
     LeastLoaded -- every assignment goes to a least-loaded known holder (the
                    routing rule, node.rs:420-436), so a known holder is never
                    over-served past its peers by more than the others' head-start.
     Drains      -- the backlog is fully assigned (liveness).
   Serve-BALANCE is NOT unconditional: it is bounded by the DISCOVERY HEAD-START —
   how many chunks are assigned before the last holder has offered. `Lag` is that
   budget. All-together (concurrent) offers keep it small (the choice set assembles
   within a round-trip); adversarial staggering makes it large. So:

     MC_windowed   Lag=1  -> SkewBound(1) holds   : all-together offers, the floor
     MC_staggered  Lag=4  -> SkewBound(1) VIOLATED : a late offerer starts behind,
                                                     skew tracks the head-start
   The skew is a FIXED head-start, independent of M, so it vanishes as M >> k
   (design §5.3's regime). A late holder is UNDER-served, never over-served, so the
   O5 OVERLOAD bound degrades gracefully. Exact balance at any ordering would need
   the full choice set at schedule time — a (timeout-bounded) barrier
   (DiscoveryBarrier.tla) or rateless reconciliation (design §7). *)
EXTENDS Naturals, FiniteSets

CONSTANTS
  Peers,   \* the neighbourhood's holders, full replication
  M,       \* chunks to fetch (unit jobs)
  Lag,     \* discovery head-start: assigns allowed before every holder has offered
  Skew     \* the claimed max-min serve bound under test

ASSUME M \in Nat /\ Lag \in Nat /\ Skew \in Nat

VARIABLES
  arrived,    \* SUBSET Peers : holders whose offer has landed (known to the scheduler)
  assigned,   \* [Peers -> Nat] : realised serve-load (the §5.3 fairness measure)
  remaining,  \* Nat : chunks not yet assigned
  lag         \* Nat : head-start budget remaining

vars == <<arrived, assigned, remaining, lag>>

MaxOf(S) == CHOOSE x \in S : \A y \in S : y <= x
MinOf(S) == CHOOSE x \in S : \A y \in S : x <= y
Loads    == { assigned[p] : p \in arrived }

\* the routing rule: p is least-loaded among the holders known so far
LeastLoadedNow(p) == \A q \in arrived : assigned[p] <= assigned[q]

TypeOK ==
  /\ arrived \subseteq Peers
  /\ assigned \in [Peers -> 0..M]
  /\ remaining \in 0..M
  /\ lag \in 0..Lag

Init ==
  /\ arrived = {}
  /\ assigned = [p \in Peers |-> 0]
  /\ remaining = M
  /\ lag = Lag

\* a holder's offer lands — incremental discovery, NO barrier, any order
Offer(p) ==
  /\ p \notin arrived
  /\ arrived' = arrived \cup {p}
  /\ UNCHANGED <<assigned, remaining, lag>>

\* assign ONE chunk to a least-loaded KNOWN holder (the shipped per-chunk rule).
\* Routing over a partial choice set is allowed only while the head-start budget
\* lasts; once spent, the model requires the choice set complete — the way
\* all-together offers bound the realised head-start.
Assign(p) ==
  /\ p \in arrived
  /\ remaining > 0
  /\ LeastLoadedNow(p)
  /\ (arrived = Peers \/ lag > 0)
  /\ assigned' = [assigned EXCEPT ![p] = @ + 1]
  /\ remaining' = remaining - 1
  /\ lag' = IF arrived = Peers THEN lag ELSE lag - 1
  /\ UNCHANGED arrived

AssignSome == \E p \in Peers : Assign(p)

Next == (\E p \in Peers : Offer(p)) \/ AssignSome

\* holders do offer; the scheduler keeps assigning while any assignment is enabled
\* (the least-loaded peer changes step to step, so WF on the existential).
Fairness ==
  /\ \A p \in Peers : WF_vars(Offer(p))
  /\ WF_vars(AssignSome)

Spec == Init /\ [][Next]_vars /\ Fairness

-----------------------------------------------------------------------------
\* --- safety: serve-load among known holders stays within the head-start bound ---
SkewBound == arrived = {} \/ (MaxOf(Loads) - MinOf(Loads) <= Skew)

\* --- liveness: the backlog drains ---
Drains == <>(remaining = 0)
=============================================================================
