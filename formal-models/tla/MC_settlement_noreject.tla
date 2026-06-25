------------------------ MODULE MC_settlement_noreject -------------------------
\* ABLATION: terminal rejections do NOT settle. The never-storable chunk 3 pins
\* peer 1's interval at position 2 forever — resume bookkeeping wedges and every
\* round re-offers the same page -> AdvanceComplete fails. NoDrop and storable-chunk
\* completeness still hold: this choice is about resume liveness, not delivery.
EXTENDS Naturals, Sequences, FiniteSets, TLC
Peers  == {1, 2}
Chunks == {1, 2, 3, 4}
Log    == 1 :> <<1, 2, 3>> @@ 2 :> <<2, 1, 4>>
Bad    == {3}
SettledOnly   == TRUE
RejectSettles == FALSE
VARIABLES intv, stored, rejected
INSTANCE IntervalSettlement
================================================================================
