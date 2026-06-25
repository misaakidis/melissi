----------------------------- MODULE MC_nhood -----------------------------
\* Neighbourhood, positive: 3 bins, K=2, every bin has 3 reachable candidates
\* (surplus over K, so the neighbourhood density rule has work to do and shallow
\* bins have surplus to shed as depth rises). Expect: every bin reaches K, the
\* neighbourhood (bins >= depth) fully connects, the working set stays <= K below
\* depth.
EXTENDS Naturals, FiniteSets, TLC
MaxBin == 2
Cand   == 0 :> 3 @@ 1 :> 3 @@ 2 :> 3
K      == 2
PrioritizeNbhd == TRUE
Pruning        == TRUE
VARIABLES conn
INSTANCE Neighbourhood
===========================================================================
