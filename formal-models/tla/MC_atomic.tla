------------------------------- MODULE MC_atomic -------------------------------
\* Atomic check-and-mark (the design's Want). Expect: ConflictFree holds.
EXTENDS Naturals, FiniteSets, TLC
Chunks == {1, 2}
Peers  == {1, 2}
Holds  == 1 :> {1,2} @@ 2 :> {1,2}            \* full replication: both peers hold both chunks
Atomic == TRUE
VARIABLES got, want, pending, conflict
INSTANCE PullSyncerNA
================================================================================
