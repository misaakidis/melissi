-------------------------- MODULE MC_nhood_noprune --------------------------
\* Ablation: Pruning OFF — a bin filled densely while it was in the neighbourhood
\* keeps that surplus after depth rises past it. Expect: Bounded violated (a bin
\* below depth holds > K forever); Saturates and NeighbourhoodComplete still hold.
EXTENDS Naturals, FiniteSets, TLC
MaxBin == 2
Cand   == 0 :> 3 @@ 1 :> 3 @@ 2 :> 3
K      == 2
PrioritizeNbhd == TRUE
Pruning        == FALSE
VARIABLES conn
INSTANCE Neighbourhood
=============================================================================
