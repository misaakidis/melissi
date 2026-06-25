------------------------------- MODULE Composition -------------------------------
(* The composition theorem, machine-checked: the supply layer and the scheduling
   layer MEET. PullSyncerE proves a node fetches every chunk GIVEN supply (design
   §3: "supply assumed"). Neighbourhood proves a node ASSEMBLES that supply —
   connects every honest neighbour (SupplyComplete). This module proves the
   hand-off: SupplyComplete + the network's redundancy (Coverage) entail
   PullSyncerE's premise, so the reserve completes. It is the seam, not a re-proof
   of either half — each sub-spec proves its mechanism in full; this shows they
   compose, and that the supply half is load-bearing for the whole.

   ABSTRACTION (assume-guarantee). The two detailed specs are abstracted to their
   GUARANTEES at the interface:
     - Neighbourhood -> the Connect action with the SAME `ConnectAll` knob: the
       node connects the honest neighbourhood (ConnectAll), or only its seed.
     - PullSyncerE -> the Fetch action: a chunk is got once SOME connected HONEST
       peer holds it (PullSyncerE's Completeness given a reachable holder, with
       failover past Byzantine/absent holders folded in).
   Discovery (the gossip feedback) is proven in Neighbourhood and assumed here
   (peers are reachable); the scheduling detail (dedup, priority, settlement) is
   proven in PullSyncerE and assumed here (a reachable holder ⇒ eventual fetch).

   COVERAGE (the network assumption, design §4 / Θ-REP). Every reserve chunk is
   held by some HONEST neighbour — the redundancy the neighbourhood maintains.
   A cold-starting node's neighbours collectively hold its reserve, though no
   single one need hold all of it (convergence in progress, or fresh uploads).

   THE KNOB (linking the two layers):
     ConnectAll -- connect every honest neighbour (the full supply) vs only the
       seed. OFF -> single-source: the reserve completes only if the seed alone
       holds all of it, which it need not — so a chunk the seed lacks is never
       fetched: Completeness fails (MC_compose_single). This is the SAME knob
       whose absence breaks SupplyComplete in Neighbourhood.tla; here its absence
       breaks Completeness downstream — that linkage IS the composition's content,
       and the single-source dependency §5.1 removes. *)
EXTENDS Naturals, FiniteSets

CONSTANTS
  Chunks,      \* the node's reserve — the chunks it must hold
  Peers,       \* its neighbourhood peers
  Holds,       \* [Peers -> SUBSET Chunks] : what each neighbour holds
  Honest,      \* SUBSET Peers : honest peers serve what they hold; the rest omit
  Seed,        \* the bootstrap peer (one honest neighbour)
  ConnectAll   \* BOOLEAN : connect the whole honest neighbourhood, vs only the seed

ASSUME Honest \subseteq Peers /\ Seed \in Honest
ASSUME Holds \in [Peers -> SUBSET Chunks]
\* Coverage: every reserve chunk is held by some honest neighbour (the redundancy
\* the neighbourhood maintains — without it no policy could complete the reserve).
ASSUME \A c \in Chunks : \E p \in Honest : c \in Holds[p]

VARIABLES
  conn,   \* SUBSET Peers   : neighbours connected (the assembled supply)
  got     \* SUBSET Chunks  : reserve chunks fetched so far

vars == <<conn, got>>

TypeOK == conn \subseteq Peers /\ got \subseteq Chunks

Init == conn = {} /\ got = {}

\* Connect a neighbour (you cannot tell honest from Byzantine a priori, so any
\* may be connected). ConnectAll keeps connecting the neighbourhood; without it
\* only the seed is ever connected — the single-source policy.
Connect(p) ==
  /\ p \in Peers /\ p \notin conn
  /\ (ConnectAll \/ p = Seed)
  /\ conn' = conn \cup {p}
  /\ UNCHANGED got

\* Fetch a chunk — PullSyncerE's guarantee, abstracted: it succeeds once a
\* connected HONEST peer holds it (a Byzantine holder serves nothing — failover).
Fetch(c) ==
  /\ c \in Chunks /\ c \notin got
  /\ \E p \in conn : p \in Honest /\ c \in Holds[p]
  /\ got' = got \cup {c}
  /\ UNCHANGED conn

Next == (\E p \in Peers : Connect(p)) \/ (\E c \in Chunks : Fetch(c))

\* Both layers' steps are obligated: the node keeps connecting neighbours and
\* fetching from connected holders until the reserve is complete.
Fairness ==
  /\ \A p \in Peers : WF_vars(Connect(p))
  /\ \A c \in Chunks : WF_vars(Fetch(c))

Spec == Init /\ [][Next]_vars /\ Fairness

-----------------------------------------------------------------------------
\* --- safety ---
\* never fetch a chunk without a connected honest holder (the supply was real).
Sound == \A c \in got : \E p \in conn : p \in Honest /\ c \in Holds[p]

\* --- liveness ---
\* the reserve completes: every chunk fetched. The end-to-end guarantee, holding
\* with Byzantine neighbours present and holdings spread across honest ones —
\* PROVIDED the full honest supply is assembled (ConnectAll).
Completeness == <>(Chunks \subseteq got)
=============================================================================
