-------------------------- MODULE MC_settlement_eager --------------------------
\* ABLATION: eager advance — master's Add(start, Topmost) when the per-peer session
\* ends, no settlement gate. Sound while each peer fetches everything it offers;
\* under cross-peer dedup the interval covers unsettled chunks, which lose
\* visibility and can never be stored -> NoDrop breaks (and completeness with it).
\* Settlement is the load-bearing difference between resume bookkeeping and
\* forgetting an unfetched chunk.
EXTENDS Naturals, Sequences, FiniteSets, TLC
Peers  == {1, 2}
Chunks == {1, 2, 3, 4}
Log    == 1 :> <<1, 2, 3>> @@ 2 :> <<2, 1, 4>>
Bad    == {3}
SettledOnly   == FALSE
RejectSettles == TRUE
VARIABLES intv, stored, rejected
INSTANCE IntervalSettlement
================================================================================
