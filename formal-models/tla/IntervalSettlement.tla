--------------------------- MODULE IntervalSettlement ---------------------------
(* Interval settlement — companion to PullSyncerE, in the PullSyncerNA mould: a
   minimal spec isolating ONE refinement obligation the implementation adds outside
   the verified scheduling machine.

   THE RULE. The interval (bee: intervalstore.Intervals per (peer, bin); Next() =
   where the next Offer starts, Add(start, x) = "never offer me this range again")
   is pull-sync's only durable claim, and advancing it is FORGETTING: a BinID the
   interval covers is never re-offered by that peer. PullSyncerE's Completeness
   quantifies over what keeps being offered, so the advance must not forget an
   unfetched chunk. Interval settlement is the gate on that one statestore write
   (implementation doc §6):

     advance (peer, bin) to the largest x <= Topmost such that EVERY chunk that peer
     offered with BinID <= x is SETTLED — stored (fetched from anyone) or terminally
     rejected (invalid stamp / ErrOverwriteNewerChunk replay: a property of the
     entry, identical at every holder, so rejection settles it globally).

   Settle before you forget. Master's semantics — advance to Topmost when the
   per-peer session ends — is EAGER advance: sound only while each peer fetches
   everything it offers, broken by cross-peer dedup (MC_settlement_eager).

   COMPOSITION. PullSyncerE proves: every chunk still offered is eventually got,
   given supply. This module proves: settled-only advance never forgets an
   unsettled chunk (NoDrop), and the intervals still drain (AdvanceComplete).
   Composing the two closes pull's completeness across the resume layer — restarts
   included, since the interval is the only state that survives one. Accordingly
   the fetch machinery is abstracted to one action: Store(c) fires only while c is
   Visible — exactly the guarantee the verified machine provides.

   BEE MAPPING. Log[p]   = what peer p OFFERS for the bin, in BinID order. Identifying
                           offers with holdings — OFFER COMPLETENESS, an offer for
                           [start, Topmost] names every entry p holds in that range —
                           is an implementation assumption, not proven here: for a
                           Byzantine p under-offering is omission (absorbed by supply;
                           the interval is per-peer), for the honest server it is
                           pinned by a server-side test (implementation doc §6).
                intv[p]  = the persisted interval high-water for (p, bin). The spec's
                           single mark is exact only under PREFIX-ONLY usage of bee's
                           intervalstore (no disconnected ranges) — likewise pinned by
                           test (implementation doc §6).
                stored   = ReserveHas (got — global, by triple);
                rejected = terminally rejected entries (never storable);
                Visible  = still offered: some peer names c above its interval.

   KNOBS (the two design choices, each ablated):
     SettledOnly   -- the settlement gate on Advance. OFF = eager advance (master
                      semantics under cross-peer dedup) -> NoDrop breaks: the
                      interval covers an unsettled chunk, which loses visibility
                      and is never stored (MC_settlement_eager).
     RejectSettles -- terminal rejections settle. OFF -> a never-storable entry
                      pins its peer's interval forever; resume bookkeeping wedges:
                      AdvanceComplete fails (MC_settlement_noreject). Completeness
                      of storable chunks still holds — the choice is about resume
                      liveness, not delivery.

   The contiguous-prefix choice needs no knob: Advance may pick ANY x inside the
   settled prefix, so TLC explores partial and full advances alike. Pagination only
   restricts x further (a subset of these behaviours) — covered. Out of scope, by
   design: epoch reset and radius-driven interval resets (interval REGRESSION — the
   Monotone property documents the assumption); LIVE log growth (the cursor boundary
   is PullSyncerE's NewChunk, not settlement's concern; Log is one HIST snapshot). *)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
  Peers, Chunks,
  Log,           \* [Peers -> Seq(Chunks)] : each peer's bin, BinID order
  Bad,           \* SUBSET Chunks : never-storable entries (invalid stamp / replay)
  SettledOnly,   \* BOOLEAN : Advance gated on settlement (the rule under test)
  RejectSettles  \* BOOLEAN : terminal rejection counts as settled

ASSUME Bad \subseteq Chunks
ASSUME \A p \in Peers : \A i \in 1..Len(Log[p]) : Log[p][i] \in Chunks
ASSUME \A c \in Chunks : \E p \in Peers : \E i \in 1..Len(Log[p]) : Log[p][i] = c
       \* every entry is offered by someone (supply, at settlement's level)

VARIABLES
  intv,        \* [Peers -> Nat] : interval high-water per peer (0 = nothing covered)
  stored,      \* SUBSET Chunks  : ReserveHas — fetched from anyone, by triple
  rejected     \* SUBSET Chunks  : terminally rejected

vars == <<intv, stored, rejected>>

TypeOK ==
  /\ intv \in [Peers -> Nat]
  /\ \A p \in Peers : intv[p] \in 0..Len(Log[p])
  /\ stored \subseteq Chunks \ Bad
  /\ rejected \subseteq Bad

Init ==
  /\ intv = [p \in Peers |-> 0]
  /\ stored = {}
  /\ rejected = {}

\* settled per the RULE (what Advance may forget)
Settled(c) == c \in stored \/ (RejectSettles /\ c \in rejected)

\* still offered: some peer's bin names c above that peer's interval
Visible(c) == \E p \in Peers : \E i \in 1..Len(Log[p]) : i > intv[p] /\ Log[p][i] = c

\* the verified machine completes a fetch — possible only while c is still offered
Store(c) ==
  /\ c \in Chunks \ Bad
  /\ c \notin stored
  /\ Visible(c)
  /\ stored' = stored \cup {c}
  /\ UNCHANGED <<intv, rejected>>

\* a fetch attempt hits a never-storable entry — detection also needs visibility
Reject(c) ==
  /\ c \in Bad
  /\ c \notin rejected
  /\ Visible(c)
  /\ rejected' = rejected \cup {c}
  /\ UNCHANGED <<intv, stored>>

\* the settlement action: intervalstore.Add(intv[p], x). SettledOnly = the rule;
\* eager (SettledOnly = FALSE) = master semantics, advance when the session ends.
Advance(p, x) ==
  /\ x \in (intv[p]+1)..Len(Log[p])
  /\ SettledOnly => \A i \in (intv[p]+1)..x : Settled(Log[p][i])
  /\ intv' = [intv EXCEPT ![p] = x]
  /\ UNCHANGED <<stored, rejected>>

AdvanceP(p) == \E x \in (intv[p]+1)..Len(Log[p]) : Advance(p, x)

Next ==
  \/ \E c \in Chunks : Store(c)
  \/ \E c \in Chunks : Reject(c)
  \/ \E p \in Peers : AdvanceP(p)

\* the puller's own steps are obligated: rounds recur (Offer/Fetch retry -> Store,
\* Reject) and the settle pass runs (Advance). Nothing here is adversarial-may.
Fairness ==
  /\ \A c \in Chunks : WF_vars(Store(c))
  /\ \A c \in Chunks : WF_vars(Reject(c))
  /\ \A p \in Peers : WF_vars(AdvanceP(p))

Spec == Init /\ [][Next]_vars /\ Fairness

-----------------------------------------------------------------------------
\* --- safety ---
\* nothing is forgotten before it settles: an interval never covers an unsettled
\* entry. (Stated over the TRUE notion — stored or rejected — not the rule's
\* Settled, so MC_settlement_noreject keeps it while losing only resume liveness.)
NoDrop == \A p \in Peers : \A i \in 1..intv[p] : Log[p][i] \in stored \cup rejected

\* --- liveness ---
Completeness    == <>((Chunks \ Bad) \subseteq stored)       \* no chunk silently dropped
AdvanceComplete == <>[](\A p \in Peers : intv[p] = Len(Log[p]))  \* resume state drains
Monotone        == [][\A p \in Peers : intv'[p] >= intv[p]]_vars \* no regression (epoch reset out of scope)
=============================================================================
