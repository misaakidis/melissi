----------------------------- MODULE Neighbourhood -----------------------------
(* Neighbourhood supply — the companion that discharges pull-sync's one external
   premise. PullSyncerE assumes SUPPLY (design §3: "supply assumed" — every
   reserve chunk is held by some honest neighbour the node can reach). This spec
   proves a node actually ASSEMBLES that supply: it discovers and connects the
   honest peers of its neighbourhood. Composing the two closes pull-sync over the
   discovery layer, the way IntervalSettlement closes it over the resume layer.

   GROUNDED IN THE DECOMPOSITION (§4). The depth-D partition splits the address
   space into 2^D neighbourhoods whose pull-sync instances are INDEPENDENT
   (locality lemma: only your depth-D neighbours hold any chunk in your reserve;
   no cross-neighbourhood serving). So the analysis is ONE neighbourhood — a tile
   of k in [2,8] peers sharing your depth-D prefix — and the supply for that tile
   is exactly its honest peers. This module is that one tile; it does not model
   proximity bins, routing, or the rest of the address space. (Routing across
   the whole space — the Kademlia k-bucket table and iterative lookup — is a
   DIFFERENT problem, used by retrieval, not pull-sync; it is deferred to its own
   formalisation. The neighbourhood is structural locality, not Kademlia routing.)

   THE COUPLING THAT MAKES IT NON-TRIVIAL. Discovery and connection depend on each
   other: a node knows only its bootstrap peer until it CONNECTS, and only a
   connected node learns more (the hive `peers` push — `net::hive`). The seed
   breaks the cycle. And not every neighbour connects: peers split into WILLING
   (honest/reachable — `Willing`) and DECLINING (`Declining`; a real testnet bee
   declines a light peer). The supply is the willing ones; the node must connect
   them ALL, not stop at the first — a single connected holder is the single-source
   dependency §5.1 removes, leaving failover nothing to fail over to.

   KNOBS (each a design choice, each ablated):
     Gossip     -- a connected node learns more neighbours (the feedback loop).
                   OFF -> never learns past the bootstrap peer -> DiscoveryFinds
                   fails (MC_nhood_nogossip). The connectivity-gated loop is what
                   discovery IS; there is no oracle pool.
     ConnectAll -- connect every honest neighbour, not merely enough to bootstrap.
                   OFF -> the node connects only its seed -> the supply is one
                   peer, not the neighbourhood -> SupplyComplete fails
                   (MC_nhood_noconnect): the single-source dependency.

   OUT OF SCOPE, by design: the proximity arithmetic that assigns a peer to this
   tile (`overlay::proximity`, pinned by vector); peer churn (the dual of the
   tile shrinking); and Kademlia routing (the deferred companion). *)
EXTENDS Naturals

CONSTANTS
  Willing,     \* Nat : honest, connectable neighbours in the tile (the supply; k-sized)
  Declining,   \* Nat : neighbours that are discoverable but decline / are unreachable
  Gossip,      \* BOOLEAN : a connected node learns more neighbours (the feedback loop)
  ConnectAll   \* BOOLEAN : connect every honest neighbour, not just enough to bootstrap

ASSUME Willing \in Nat /\ Willing >= 1   \* the bootstrap peer is one willing neighbour
ASSUME Declining \in Nat
ASSUME Gossip \in BOOLEAN /\ ConnectAll \in BOOLEAN

VARIABLES
  knownW,  \* Nat : willing neighbours DISCOVERED so far (<= Willing)
  knownU,  \* Nat : declining neighbours discovered (<= Declining) — never connectable
  conn     \* Nat : willing neighbours CONNECTED (<= knownW) — the assembled supply

vars == <<knownW, knownU, conn>>

\* connected to at least one neighbour: the precondition for learning more (you
\* must be in the neighbourhood to discover the neighbourhood).
Bootstrapped == conn > 0

TypeOK ==
  /\ knownW \in 0..Willing /\ knownU \in 0..Declining /\ conn \in 0..Willing
  /\ conn <= knownW

Init ==
  /\ knownW = 1            \* only the bootstrap peer is known
  /\ knownU = 0
  /\ conn   = 0

\* Discovery (the hive push): a connected node learns one more neighbour, willing
\* or declining. Gated on Gossip AND Bootstrapped — no feedback, or no connection
\* yet, means nothing past the seed is ever learned.
DiscoverW ==
  /\ Gossip /\ Bootstrapped
  /\ knownW < Willing
  /\ knownW' = knownW + 1
  /\ UNCHANGED <<knownU, conn>>
DiscoverU ==
  /\ Gossip /\ Bootstrapped
  /\ knownU < Declining
  /\ knownU' = knownU + 1
  /\ UNCHANGED <<knownW, conn>>

\* Connect one more known willing neighbour. With ConnectAll, keep connecting the
\* whole honest neighbourhood; without it, connect only enough to bootstrap (the
\* first one) and then stop — the single-source policy.
Connect ==
  /\ conn < knownW
  /\ (ConnectAll \/ ~Bootstrapped)
  /\ conn' = conn + 1
  /\ UNCHANGED <<knownW, knownU>>

Next == DiscoverW \/ DiscoverU \/ Connect

\* The node's own steps are obligated: it keeps learning and dialling until the
\* neighbourhood is assembled. Nothing here is adversarial-may.
Fairness ==
  /\ WF_vars(DiscoverW)
  /\ WF_vars(DiscoverU)
  /\ WF_vars(Connect)

Spec == Init /\ [][Next]_vars /\ Fairness

-----------------------------------------------------------------------------
\* --- safety ---
\* never connect a neighbour that was not discovered-and-willing.
ConnLeKnown == conn <= knownW /\ knownW <= Willing

\* --- liveness ---
\* discovery finds the whole neighbourhood — every willing and declining peer —
\* once the node has bootstrapped. The property the feedback loop earns.
DiscoveryFinds == <>[](knownW = Willing /\ knownU = Declining)

\* the supply is COMPLETE: the node connects every honest neighbour, so every
\* reserve chunk an honest neighbour holds has a connected holder — exactly the
\* premise PullSyncerE assumes, and the redundancy §5.1's failover needs.
SupplyComplete == <>[](conn = Willing)
=============================================================================
