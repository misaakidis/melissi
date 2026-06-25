------------------------------- MODULE MC_pacing -------------------------------
\* Offer pacing, positive: paced emission with two ticks and two arrivals.
\* Expect: AdvertBound (adverts = justifications) AND Drained (pacing is not
\* too strong — coverage and settlement still drain to the head).
EXTENDS Naturals, TLC
Paced        == TRUE
Griefer      == FALSE
TickBudget   == 2
GrowthBudget == 2
VARIABLES head, covered, intv, open, ticks, growth, noffers, just
INSTANCE OfferPacing
================================================================================
