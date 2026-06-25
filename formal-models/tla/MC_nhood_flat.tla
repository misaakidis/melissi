--------------------------- MODULE MC_nhood_flat ---------------------------
\* Ablation: PrioritizeNbhd OFF — a flat "K per bin everywhere" policy. The
\* neighbourhood bins stall at K < Cand, so they are never densely connected.
\* Expect: NeighbourhoodComplete violated (Saturates and Bounded still hold).
EXTENDS Naturals, FiniteSets, TLC
MaxBin == 2
Cand   == 0 :> 3 @@ 1 :> 3 @@ 2 :> 3
K      == 2
PrioritizeNbhd == FALSE
Pruning        == TRUE
VARIABLES conn
INSTANCE Neighbourhood
============================================================================
