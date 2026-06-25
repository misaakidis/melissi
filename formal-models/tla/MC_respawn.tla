------------------------------ MODULE MC_respawn ------------------------------
\* ABLATION: unpaced emission — the shell re-advertises covered-but-unsettled
\* ground as the immediate consequence of a round completing (the eager
\* pattern; wall-clock shells exhibit it as the §6.2 empty-offer respawn).
\* Expect: AdvertBound violated within three steps — emit, answer, emit.
EXTENDS Naturals, TLC
Paced        == FALSE
Griefer      == FALSE
TickBudget   == 0
GrowthBudget == 0
VARIABLES head, covered, intv, open, ticks, growth, noffers, just
INSTANCE OfferPacing
================================================================================
