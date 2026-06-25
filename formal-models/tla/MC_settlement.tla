----------------------------- MODULE MC_settlement -----------------------------
\* Interval settlement, positive: cross-peer overlap (chunks 1,2 in both bins, in
\* DIFFERENT BinID positions — the fetched-from-the-other-peer case), one
\* never-storable entry (chunk 3, e.g. invalid stamp), one single-source chunk (4).
\* Expect: NoDrop holds, every storable chunk stored, both intervals fully drain.
EXTENDS Naturals, Sequences, FiniteSets, TLC
Peers  == {1, 2}
Chunks == {1, 2, 3, 4}
Log    == 1 :> <<1, 2, 3>> @@ 2 :> <<2, 1, 4>>
Bad    == {3}
SettledOnly   == TRUE
RejectSettles == TRUE
VARIABLES intv, stored, rejected
INSTANCE IntervalSettlement
================================================================================
