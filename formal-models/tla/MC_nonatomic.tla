----------------------------- MODULE MC_nonatomic -----------------------------
\* Non-atomic check-then-mark (Decide / Commit split). Two peers both pass the in-flight
\* check on want[c]={} and both mark -> the chunk is delivered twice -> ConflictFree ✗.
\* Shows the in-flight check-and-mark must be a single critical section in the implementation.
EXTENDS Naturals, FiniteSets, TLC
Chunks == {1, 2}
Peers  == {1, 2}
Holds  == 1 :> {1,2} @@ 2 :> {1,2}            \* full replication: both peers hold both chunks
Atomic == FALSE
VARIABLES got, want, pending, conflict
INSTANCE PullSyncerNA
================================================================================
