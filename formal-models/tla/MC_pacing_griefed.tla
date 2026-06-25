--------------------------- MODULE MC_pacing_griefed ---------------------------
\* The §6.2 closure, machine-checked: a server answering instantly-empty
\* swallows credits but can never extort adverts beyond justifications —
\* AdvertBound holds; the respawn rate is bounded by the tick cadence (the
\* "explicit per-round floor"). Liveness for this peer is deliberately NOT
\* asserted: an always-empty answerer stalls this peer's sync, and the system
\* answer is failover (supply from other peers), not pacing.
EXTENDS Naturals, TLC
Paced        == TRUE
Griefer      == TRUE
TickBudget   == 2
GrowthBudget == 2
VARIABLES head, covered, intv, open, ticks, growth, noffers, just
INSTANCE OfferPacing
================================================================================
