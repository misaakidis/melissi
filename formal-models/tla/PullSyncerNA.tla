------------------------------ MODULE PullSyncerNA ------------------------------
(* SWIP-25E atomicity ablation — a companion to PullSyncerE that isolates the ONE
   concurrency obligation the design places on its implementation: the in-flight
   "check, then mark" must be a single critical section.

   In PullSyncerE, `Want` marks a chunk in-flight in a single atomic step — the dedup
   guard `want[c] = {}` and the insertion happen indivisibly. A real implementation does
   check-then-act across statements. This module makes that split a CONSTANT knob:

     Atomic = TRUE   -- check and mark fuse into one step (= PullSyncerE's Want).
     Atomic = FALSE  -- Decide (check passes against want[c]) and Commit (mark in-flight)
                        are separate steps, so two peers can both pass the check on
                        want[c] = {} and both commit -> the chunk is delivered twice.

   Verifies the SAME safety property as the main testbed:
     ConflictFree -- no chunk delivered twice (exactly-once, O3).

   Result: Atomic=TRUE -> ConflictFree holds (MC_atomic); Atomic=FALSE -> ConflictFree
   breaks (MC_nonatomic). This is an implementation-refinement obligation, not a design
   knob — hence a separate, minimal spec. Full replication (>= 2 holders/chunk) is what
   makes the race reachable: several peers offer the same chunk concurrently. *)
EXTENDS Naturals, FiniteSets

CONSTANTS Chunks, Peers, Holds, Atomic

VARIABLES
  got,        \* SUBSET Chunks : fetched + stored
  want,       \* [Chunks -> SUBSET Peers] : in-flight claims
  pending,    \* [Chunks -> SUBSET Peers] : Decided-but-not-yet-Committed (only when ~Atomic)
  conflict    \* BOOLEAN : latched on a double-delivery

vars == <<got, want, pending, conflict>>

TypeOK ==
  /\ got \subseteq Chunks
  /\ want \in [Chunks -> SUBSET Peers]
  /\ pending \in [Chunks -> SUBSET Peers]
  /\ conflict \in BOOLEAN

Init ==
  /\ got = {}
  /\ want = [c \in Chunks |-> {}]
  /\ pending = [c \in Chunks |-> {}]
  /\ conflict = FALSE

\* the dedup check: chunk not yet in-flight from anyone
CheckOK(c) == want[c] = {}

\* ATOMIC: check and mark in one indivisible step (this is PullSyncerE's Want).
WantAtomic(c, p) ==
  /\ Atomic
  /\ c \in Holds[p]
  /\ c \notin got
  /\ p \notin want[c]
  /\ CheckOK(c)
  /\ want' = [want EXCEPT ![c] = @ \cup {p}]
  /\ UNCHANGED <<got, pending, conflict>>

\* NON-ATOMIC: the check (Decide) and the mark (Commit) are separate steps. The check
\* is evaluated against the want set as it stands at Decide time; another peer can Decide
\* in between (TOCTOU), because the mark hasn't happened yet.
Decide(c, p) ==
  /\ ~Atomic
  /\ c \in Holds[p]
  /\ c \notin got
  /\ p \notin want[c]
  /\ p \notin pending[c]
  /\ CheckOK(c)
  /\ pending' = [pending EXCEPT ![c] = @ \cup {p}]
  /\ UNCHANGED <<got, want, conflict>>

Commit(c, p) ==
  /\ ~Atomic
  /\ p \in pending[c]
  /\ want'    = [want    EXCEPT ![c] = @ \cup {p}]   \* mark in-flight WITHOUT re-checking
  /\ pending' = [pending EXCEPT ![c] = @ \ {p}]
  /\ UNCHANGED <<got, conflict>>

Deliver(c, p) ==
  /\ p \in want[c]
  /\ got' = got \cup {c}
  /\ conflict' = (conflict \/ (c \in got))           \* second delivery latches CONFLICT
  /\ want' = [want EXCEPT ![c] = @ \ {p}]
  /\ UNCHANGED <<pending>>

Next ==
  \/ \E c \in Chunks, p \in Peers : WantAtomic(c, p)
  \/ \E c \in Chunks, p \in Peers : Decide(c, p)
  \/ \E c \in Chunks, p \in Peers : Commit(c, p)
  \/ \E c \in Chunks, p \in Peers : Deliver(c, p)

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
ConflictFree == conflict = FALSE             \* exactly-once delivery (O3)
=============================================================================
