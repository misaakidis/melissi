------------------------------- MODULE OfferPacing -------------------------------
(* Offer pacing — companion in the PullSyncerNA mould: the advertisement-round
   rule the other specs cannot see, because neither models the advertisement
   step itself (PullSyncerE abstracts offers into holds/arrived; settlement's
   Visible ASSUMES re-offering recurs). Found by the deterministic harness:
   a sans-io shell that re-advertises covered-but-unsettled ground busy-loops;
   a wall-clock shell dilutes the same bug into the §6.2 empty-offer respawn,
   self-inflicted — an empty offer is instant and costs the rate limiter
   nothing.

   THE RULE, as a floor. Every Offer must be JUSTIFIED: the initial round, a
   gap closing into a fresh uncovered tail (the interval catching the covered
   high-water: Next > covered becomes true), or a paced tick (the shell-owned
   same-range refresh: churn detection). Arrivals need no justification — the
   standing tail offer covers them; the serving side blocks on an empty range.
   `AdvertBound == noffers <= just` is the advertisement analogue of
   DeliveryFloor (deliveries = chunks stored): adverts = justifications.
   Unpaced emission (the ablation) violates it within three steps.

   GRIEFING NOTE (§6.2 closure). A credit is consumed per emission and
   re-granted only by a TRANSITION (gap-close) or a tick — so a Byzantine
   server answering instantly-empty swallows one credit without re-justifying:
   the respawn rate is bounded by the tick cadence, which is the "explicit
   per-round floor" §6.2 names as absent today.

   One (peer, bin) suffices: pacing is per-log, and logs are independent.

   BEE MAPPING. covered = the highest answered Topmost (SOFT state — beside
   the durable interval, making the resume state a pair); intv = the persisted
   interval; open = the one-open-offer-per-(peer,bin) flag; Tick = the floored
   poll tick; just/noffers = the metric pair to ship (a respawn tripwire). *)
EXTENDS Naturals

CONSTANTS
  Paced,        \* BOOLEAN : Emit requires a justification (the rule under test)
  Griefer,      \* BOOLEAN : the server may answer instantly-empty (§6.2)
  TickBudget,   \* Nat : paced same-range refreshes available
  GrowthBudget  \* Nat : LIVE arrivals at the serving peer

VARIABLES
  head,      \* the peer's bin head (grows with arrivals)
  covered,   \* the highest Topmost an answered offer has covered (soft)
  intv,      \* the settled high-water (the durable interval); intv <= covered
  open,      \* an offer is in flight for this (peer, bin)
  ticks,     \* tick budget remaining
  growth,    \* arrival budget remaining
  noffers,   \* offers emitted — the cost counter
  just       \* justifications accrued — initial + gap-closes + ticks

vars == <<head, covered, intv, open, ticks, growth, noffers, just>>

NextOff  == intv + 1            \* where the next Offer starts (bee: Next())
TailOpen == NextOff > covered   \* uncovered ground exists

TypeOK ==
  /\ head \in Nat /\ covered \in 0..head /\ intv \in 0..covered
  /\ open \in BOOLEAN
  /\ ticks \in 0..TickBudget /\ growth \in 0..GrowthBudget
  /\ noffers \in Nat /\ just \in Nat

Init ==
  /\ head = 1 /\ covered = 0 /\ intv = 0 /\ open = FALSE
  /\ ticks = TickBudget /\ growth = GrowthBudget
  /\ noffers = 0 /\ just = 1                  \* the initial round is justified

\* the shell opens an offer; Paced => only with a justification in hand
Emit ==
  /\ ~open
  /\ Paced => noffers < just
  /\ open' = TRUE
  /\ noffers' = noffers + 1
  /\ UNCHANGED <<head, covered, intv, ticks, growth, just>>

\* the HONEST server answers only when there is coverage to give — on an
\* empty range it BLOCKS (the live subscription; bee: makeOffer waits).
\* Covering only ever CLOSES the tail (TailOpen T -> F), so an answer never
\* justifies the next emit.
Answer ==
  /\ open
  /\ head > covered
  /\ open' = FALSE
  /\ covered' = head
  /\ UNCHANGED <<head, intv, ticks, growth, noffers, just>>

\* the GRIEFER answers instantly-empty (§6.2 empty-offer respawn), swallowing
\* the credit without re-justifying — may happen, never must (no fairness).
\* AdvertBound survives it: the respawn rate is bounded by the tick cadence.
\* What it costs is liveness for THIS peer (Drained needs unbounded ticks) —
\* the system answer is failover at the machine layer: supply from others.
AnswerEmpty ==
  /\ Griefer
  /\ open
  /\ open' = FALSE
  /\ UNCHANGED <<head, covered, intv, ticks, growth, noffers, just>>

\* a chunk in covered ground settles; if the interval catches the covered
\* high-water, a fresh tail opens — the gap-close justification.
Settle ==
  /\ intv < covered
  /\ intv' = intv + 1
  /\ just' = IF (intv + 2 > covered) /\ ~(intv + 1 > covered)
       THEN just + 1                          \* TailOpen became true: credit
       ELSE just
  /\ UNCHANGED <<head, covered, open, ticks, growth, noffers>>

\* a LIVE arrival lands at the peer. NO credit: the standing tail offer
\* (already justified) covers it — the serving side blocks until it lands.
Grow ==
  /\ growth > 0
  /\ head' = head + 1
  /\ growth' = growth - 1
  /\ UNCHANGED <<covered, intv, open, ticks, noffers, just>>

\* the shell's floored poll tick: the only same-range re-justification.
Tick ==
  /\ ticks > 0
  /\ ticks' = ticks - 1
  /\ just' = just + 1
  /\ UNCHANGED <<head, covered, intv, open, growth, noffers>>

Next == Emit \/ Answer \/ AnswerEmpty \/ Settle \/ Grow \/ Tick

\* the shell and the settle pass are obligated; ticks and arrivals may happen.
Fairness ==
  /\ WF_vars(Emit) /\ WF_vars(Answer) /\ WF_vars(Settle) /\ WF_vars(Grow)

Spec == Init /\ [][Next]_vars /\ Fairness

-----------------------------------------------------------------------------
\* the floor: adverts never exceed justifications (cf. DeliveryFloor)
AdvertBound == noffers <= just

\* pacing is not too strong: coverage and settlement still drain fully
Drained == <>[](intv = head)
=============================================================================
