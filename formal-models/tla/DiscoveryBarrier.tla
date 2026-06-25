---------------------------- MODULE DiscoveryBarrier ----------------------------
(* The discovery barrier — companion to PullSyncerE / OfferPacing, isolating ONE
   liveness obligation neither can see. OfferPacing models a single (peer, bin)
   and pins a WITHHOLDING server's cost to itself, ASSUMING "the system answer is
   failover at the machine layer: supply from others". PullSyncerE has no offer
   layer at all. The barrier is precisely the cross-peer coupling that VOIDS that
   assumption: it gates a whole bin's scheduling on EVERY lister resolving, so a
   peer that completes the cursor handshake and then never offers (offline, or
   malicious — the puller cannot tell, exactly as ByzStall == SpuriousTimeout)
   stops being its own problem and STALLS the honest peers in its bin.

   THE RULE under test. Schedule fetches in a bin only when its choice set is
   assembled — every cursored lister has resolved its HIST range (answered, or
   empty). That is the O5 device (don't hand the first responder the whole wave).
   This module asks the orthogonal question: is the barrier LIVE? It abstracts
   chunks away — "scheduled p" stands for "the pivot issued a fetch to honest
   holder p"; chunk completeness then follows from PullSyncerE GIVEN scheduling
   happens. So the only thing modelled is: does scheduling happen at all, for the
   honest holders, when one lister withholds.

   BEE / melissi MAPPING. cursored = mark_cursored (GetCursors returned);
   resolved = hist_resolved (first Offer answered, or empty cursor); evicted =
   the shell dropping a stalled lister — PeerGone (forget_peer) OR a timed-out
   empty OfferResult, the two ways node.rs clears the block; sched = a Fetch
   effect emitted to an honest peer; BinReady = discovery.rs:bin_ready.

   KNOBS (each ablated):
     Barrier  -- the gate. ON  = wait for the whole choice set (the shipped
                 policy.discovery_barrier). OFF = schedule each holder as it
                 resolves (commit-on-offer: "tick the missing chunks on the next
                 WANT"). OFF needs no timeout to be live; it trades the O5 floor
                 (a DISTRIBUTIONAL property — measured in the sim, not here).
     Timeout  -- TimeoutBudget: the shell may evict a pending lister this many
                 times (a bounded offer-timeout / peer-drop). 0 = no timeout:
                 the pure barrier, which WEDGES on a withholder. >0 = the wait is
                 bounded, progress restored at a latency cost (sim-measured).

   THE RESULT this model delivers:
     Barrier OFF, any Timeout            -> Progress  (no dependence on timeout)
     Barrier ON,  Timeout >= |Withhold|  -> Progress  (the fix: barrier + drop)
     Barrier ON,  Timeout  = 0, Withhold -> Progress VIOLATED  (the wedge)
   So the barrier is live ONLY if paired with a timeout; without one it converts
   a withholding peer — which failover handles trivially when there is no barrier
   — into a bin-wide stall. That is the formal statement of the burstability /
   anti-stall trade: dropping the barrier spends the analytic O5 floor to buy a
   liveness guarantee the verified machine does not otherwise provide. *)
EXTENDS Naturals, FiniteSets

CONSTANTS
  Peers,          \* the neighbourhood, for one bin
  Withholders,    \* SUBSET Peers : cursor but NEVER offer (offline or malicious)
  Barrier,        \* BOOLEAN : gate scheduling on the full choice set
  TimeoutBudget   \* Nat : bounded evictions of a pending lister (0 = no timeout)

Honest == Peers \ Withholders

ASSUME Withholders \subseteq Peers
ASSUME Honest # {}                       \* supply: at least one honest holder
ASSUME Barrier \in BOOLEAN
ASSUME TimeoutBudget \in Nat

VARIABLES
  cursored,   \* SUBSET Peers : have returned cursors (and so "list the bin")
  resolved,   \* SUBSET Peers : HIST range resolved (offer answered / empty)
  evicted,    \* SUBSET Peers : dropped by the shell timeout (PeerGone / empty)
  sched,      \* SUBSET Peers : honest holders the pivot has scheduled a fetch to
  tmo         \* Nat : eviction budget remaining

vars == <<cursored, resolved, evicted, sched, tmo>>

TypeOK ==
  /\ cursored \subseteq Peers
  /\ resolved \subseteq cursored
  /\ evicted  \subseteq cursored
  /\ sched    \subseteq Honest
  /\ tmo \in 0..TimeoutBudget

Init ==
  /\ cursored = {}
  /\ resolved = {}
  /\ evicted  = {}
  /\ sched    = {}
  /\ tmo = TimeoutBudget

\* the choice set is assembled: every cursored lister NOT evicted has resolved.
\* Barrier OFF collapses the gate (each resolved holder is schedulable at once).
BinReady ==
  \/ ~Barrier
  \/ \A q \in cursored \ evicted : q \in resolved

\* GetCursors returns for p (honest AND withholders complete the handshake).
Cursor(p) ==
  /\ p \notin cursored
  /\ cursored' = cursored \cup {p}
  /\ UNCHANGED <<resolved, evicted, sched, tmo>>

\* an honest lister answers its offer (or it was empty) -> resolved.
\* Withholders are exactly the peers for which this action never fires.
Resolve(p) ==
  /\ p \in cursored
  /\ p \in Honest
  /\ p \notin resolved
  /\ resolved' = resolved \cup {p}
  /\ UNCHANGED <<cursored, evicted, sched, tmo>>

\* the shell evicts a pending lister: a timed-out offer (empty) or PeerGone.
\* Bounded by TimeoutBudget — the analogue of PullSyncerE's spurious-timeout
\* budget: a misfire on a slow-honest peer is possible but never obligated to
\* be free. Removing the withholder from the readiness quantifier is what lets
\* a barrier'd bin make progress.
Evict(p) ==
  /\ tmo > 0
  /\ p \in cursored
  /\ p \notin resolved
  /\ p \notin evicted                      \* don't re-evict (would just burn budget)
  /\ evicted' = evicted \cup {p}
  /\ tmo' = tmo - 1
  /\ UNCHANGED <<cursored, resolved, sched>>

\* the pivot schedules a fetch to honest holder p — possible once p has offered
\* (p \in resolved) and, under Barrier, once the whole choice set is in.
Schedule(p) ==
  /\ p \in Honest
  /\ p \in resolved
  /\ p \notin sched
  /\ BinReady
  /\ sched' = sched \cup {p}
  /\ UNCHANGED <<cursored, resolved, evicted, tmo>>

Next ==
  \/ \E p \in Peers : Cursor(p)
  \/ \E p \in Peers : Resolve(p)
  \/ \E p \in Peers : Evict(p)
  \/ \E p \in Peers : Schedule(p)

\* the protocol's own steps are obligated: peers do cursor, honest peers do
\* answer, the cooldown does expire (WF on Evict — like WF on ResetExcluded),
\* and the pivot does schedule whatever is enabled. NOTHING here is adversarial:
\* the adversary is the STATIC Withholders set, which simply never Resolves.
Fairness ==
  /\ \A p \in Peers : WF_vars(Cursor(p))
  /\ \A p \in Peers : WF_vars(Resolve(p))
  /\ \A p \in Peers : WF_vars(Evict(p))
  /\ \A p \in Peers : WF_vars(Schedule(p))

Spec == Init /\ [][Next]_vars /\ Fairness

-----------------------------------------------------------------------------
\* NB: "the barrier schedules only with the full choice set" is NOT a state
\* invariant — readiness is non-monotonic. A peer may return its cursor AFTER an
\* honest holder was scheduled on the then-complete set, flipping BinReady back to
\* false (checked: a NoEarlySchedule == Barrier => (sched # {} => BinReady) is
\* violated within a few steps). So the barrier does not even guarantee a complete
\* choice set under late cursor arrival — a further argument against it. The
\* property that distinguishes the designs is purely the liveness below.

\* --- liveness: every honest holder is eventually scheduled (the bin drains
\*     from its honest supply). This is what the wedge breaks. ---
Progress == <>(Honest \subseteq sched)
=============================================================================
