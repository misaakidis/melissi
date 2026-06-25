------------------------------ MODULE PullSyncerE ------------------------------
(* SWIP-25E correctness testbed — ONE flat neighbourhood (the decomposition atom of
   pullsync-optimal-design.md §4). A pivot fills its reserve from `Peers` holder-
   peers under the REACTIVE strategy, with the design-space axes as CONSTANT knobs so
   strategies and ablations are *checked*, not assumed:

     Dedup          -- chunk-level in-flight claim set (§5.2)
     Failover       -- release a stalled claim so another holder can serve (§5.4)
     Exclude        -- on failover, also bar the staller for that chunk (§5.4)
     ResetOnExhaust -- when every current holder of a chunk is barred, clear that
                       chunk's bars (cooldown expiry / fresh retry round) (§5.4)
     SingleSource   -- restrict each chunk to one assigned source (single-primary family)
     Priority       -- vicinity-first ordering (§5.5): start deeper (higher-Prio) chunks first
     EnableLive     -- fetch post-cutoff (LIVE) arrivals (§5.6)

   Two budgets turn the model's idealisations themselves into knobs, so each assumption
   is *located*, not silently global:

     TimeoutBudget  -- max spurious timeouts: the puller's stall detector may misfire on
                       an honest, live peer at most this often. 0 = perfect stall
                       attribution (the idealised model); > 0 = asynchrony-realistic,
                       where slow-honest and Byzantine-stall are indistinguishable.
     ChurnBudget    -- max churn events: holders may lose/gain chunks at most this often,
                       supply (>= 1 honest holder per chunk) preserved throughout.
                       0 = static holdings; > 0 = bounded churn (quiescence assumption).

   HIST vs LIVE: HistChunks are the backlog (available at Init); LiveChunks arrive *during*
   sync via NewChunk (post-cutoff). The claim set / dedup / failover span both regimes by
   construction (one shared `want`), so ConflictFree covers a concurrent HIST+LIVE offer.

   Adversary: `Byzantine` peers OMIT (never deliver) and may claim-stall. Supply (O1 scope):
   every chunk is on >= 1 honest holder — an Init assumption and, under churn, a checked
   invariant (SupplyInv). Holdings are arbitrary subsets (O6b).

   Verifies CORRECTNESS (quantitative O2/O4/O5 + the vicinity-first durability trajectory
   are analytic, not model-checkable):
     ConflictFree     -- exactly-once delivery (O3 precondition)
     DeliveryFloor    -- deliveries = chunks stored: the O3 floor as an explicit invariant
     SupplyInv        -- churn never strips a chunk of its last honest holder
     DedupInv         -- with Dedup on, at most one in-flight claim per chunk
     ClaimsLive       -- every claim is actionable: holder still has the chunk, isn't barred
     NoFalseExclusion -- with TimeoutBudget = 0, only Byzantine peers are ever barred
                         (the perfect-attribution idealisation, stated as an invariant)
     Completeness     -- Phi -> 0: every chunk (incl. arrived LIVE) fetched (O1 gate + O6c)
     Freshness        -- every post-cutoff LIVE arrival is eventually delivered (§5.6)
     Quiescence       -- eventually no claim is left in flight (no leaked claims)
     Monotone         -- the store only grows (delivery is hash-verified, never undone)
   Global Theta-REP follows by the composition theorem (design doc §4), NOT modeled here. *)
EXTENDS Naturals, FiniteSets

CONSTANTS
  Chunks, Peers, Holds, Byzantine,
  Dedup, Failover, Exclude, SingleSource, Assign,
  ResetOnExhaust,\* BOOLEAN : clear a chunk's bars once they cover all current holders
  Priority,      \* BOOLEAN : vicinity-first -- don't start a chunk until higher-Prio (arrived) ones are
  Prio,          \* [Chunks -> Nat] : priority (higher = deeper/nearer to pivot = pulled first)
  EnableLive,    \* BOOLEAN : fetch LIVE (post-cutoff) arrivals
  LiveChunks,    \* SUBSET Chunks : chunks that arrive *during* sync (the rest are the backlog)
  TimeoutBudget, \* Nat : max spurious timeouts on honest peers (0 = perfect attribution)
  ChurnBudget    \* Nat : max holdings-churn events (0 = static holdings)

Honest    == Peers \ Byzantine
HistChunks == Chunks \ LiveChunks

ASSUME \A c \in Chunks : \E p \in Honest : c \in Holds[p]   \* supply (O1 scope) at Init
ASSUME Assign \in [Chunks -> Peers]
ASSUME Prio \in [Chunks -> Nat]
ASSUME LiveChunks \subseteq Chunks
ASSUME TimeoutBudget \in Nat /\ ChurnBudget \in Nat

VARIABLES
  got,        \* SUBSET Chunks : fetched + stored
  want,       \* [Chunks -> SUBSET Peers] : in-flight Wants (the claim set; spans HIST+LIVE)
  failed,     \* [Chunks -> SUBSET Peers] : peers failover gave up on (barred per chunk)
  arrived,    \* SUBSET Chunks : chunks available to fetch (HistChunks at Init; LIVE added later)
  conflict,   \* BOOLEAN : latched on a double-delivery
  holds,      \* [Peers -> SUBSET Chunks] : current holdings (= Holds until churn moves them)
  tmo,        \* Nat : spurious-timeout budget remaining
  chn,        \* Nat : churn budget remaining
  ndeliv      \* Nat : deliveries so far (DeliveryFloor: stays = Cardinality(got))

vars == <<got, want, failed, arrived, conflict, holds, tmo, chn, ndeliv>>

TypeOK ==
  /\ got \subseteq Chunks
  /\ want \in [Chunks -> SUBSET Peers]
  /\ failed \in [Chunks -> SUBSET Peers]
  /\ arrived \subseteq Chunks
  /\ conflict \in BOOLEAN
  /\ holds \in [Peers -> SUBSET Chunks]
  /\ tmo \in 0..TimeoutBudget
  /\ chn \in 0..ChurnBudget
  /\ ndeliv \in Nat

Init ==
  /\ got = {}
  /\ want = [c \in Chunks |-> {}]
  /\ failed = [c \in Chunks |-> {}]
  /\ arrived = HistChunks                  \* backlog available; LIVE chunks not yet arrived
  /\ conflict = FALSE
  /\ holds = Holds
  /\ tmo = TimeoutBudget
  /\ chn = ChurnBudget
  /\ ndeliv = 0

\* a chunk is "addressed" once we've started (wanted) or finished it
Addressed(c) == c \in got \/ want[c] # {}

\* vicinity-first: don't START c while a higher-priority *arrived* chunk is unaddressed
PrioOK(c) ==
  \/ ~Priority
  \/ \A d \in arrived : (Prio[d] > Prio[c]) => Addressed(d)

Claimable(c, p) ==
  /\ c \in arrived                         \* can't want what hasn't arrived (HIST always; LIVE after NewChunk)
  /\ c \in holds[p]
  /\ c \notin got
  /\ p \notin want[c]
  /\ p \notin failed[c]
  /\ (Dedup => want[c] = {})
  /\ (SingleSource => p = Assign[c])
  /\ (c \in LiveChunks => EnableLive)      \* fetch post-cutoff arrivals only if LIVE enabled
  /\ PrioOK(c)

\* ---------------------------------------------------------------------------
\* Per-chunk state machine.  HIST chunks start at `available`, LIVE at `unarrived`;
\* `got` is terminal (delivery is hash-verified, so it is never undone).
\*
\*   unarrived --[NewChunk]--> available --[Want]--> wanted --[Deliver]--> got
\*                             ^                    |
\*                             |                    |
\*                             +-----[ByzStall]------+      Byzantine p, only when Failover:
\*                             |                    |       releases the claim back to
\*                             +--[SpuriousTimeout]--+      `available`; if Exclude, bars p
\*                                                          for c (one-shot, no re-grab).
\*   SpuriousTimeout is the same timeout machinery misfiring on an HONEST p (the detector
\*   cannot tell slow-honest from Byzantine-stall) -- at most TimeoutBudget times.
\*   ResetExcluded re-opens a chunk whose bars cover every current holder.
\* ---------------------------------------------------------------------------

Want(c, p) ==
  /\ Claimable(c, p)
  /\ want' = [want EXCEPT ![c] = @ \cup {p}]
  /\ UNCHANGED <<got, failed, arrived, conflict, holds, tmo, chn, ndeliv>>

Deliver(c, p) ==
  /\ p \in Honest
  /\ p \in want[c]
  /\ got' = got \cup {c}
  /\ conflict' = (conflict \/ (c \in got))
  /\ want' = [want EXCEPT ![c] = @ \ {p}]
  /\ ndeliv' = ndeliv + 1
  /\ UNCHANGED <<failed, arrived, holds, tmo, chn>>

\* Byzantine claim-stall + failover: p grabbed c's claim (via Want) and never Delivers.
ByzStall(c, p) ==
  /\ p \in Byzantine                          \* an adversary stalls by intent ...
  /\ p \in want[c]                            \* p currently holds the in-flight claim for c
  /\ Failover                                 \* gate: Failover OFF disables this action, so the claim
                                              \*       stays stuck on p forever (the MC_nofailover ablation)
  /\ want'   = [want   EXCEPT ![c] = @ \ {p}]  \* release the claim -> c returns to `available`
  /\ failed' = IF Exclude                      \* Exclude ON: bar p for c (one-shot), so it cannot re-grab
       THEN [failed EXCEPT ![c] = @ \cup {p}]  \*   -> at most one stall per holder per round, <= k total
       ELSE failed                             \* Exclude OFF: p stays selectable -> claim-stall livelock
  /\ UNCHANGED <<got, arrived, conflict, holds, tmo, chn, ndeliv>>   \* (the MC_noexclude ablation)

\* ... and the same timeout MISFIRES on an honest, live peer: the puller cannot tell
\* slow-honest from Byzantine-stall, so the failover machinery treats both identically.
\* Bounded by TimeoutBudget (timeouts are transiently spurious, not forever).
SpuriousTimeout(c, p) ==
  /\ tmo > 0
  /\ p \in Honest
  /\ p \in want[c]
  /\ Failover
  /\ tmo'    = tmo - 1
  /\ want'   = [want EXCEPT ![c] = @ \ {p}]
  /\ failed' = IF Exclude
       THEN [failed EXCEPT ![c] = @ \cup {p}]  \* misattribution: an honest holder is barred
       ELSE failed
  /\ UNCHANGED <<got, arrived, conflict, holds, chn, ndeliv>>

\* candidate set exhausted -> clear the chunk's bars (cooldown expiry / fresh retry round).
\* Without this (ResetOnExhaust OFF), one misfire on a chunk's only honest holder makes the
\* chunk permanently unfetchable -- the MC_noreset ablation.
ResetExcluded(c) ==
  /\ ResetOnExhaust
  /\ c \in arrived
  /\ c \notin got
  /\ want[c] = {}
  /\ failed[c] # {}
  /\ \A p \in Peers : c \in holds[p] => p \in failed[c]
  /\ failed' = [failed EXCEPT ![c] = {}]
  /\ UNCHANGED <<got, want, arrived, conflict, holds, tmo, chn, ndeliv>>

\* churn: holder p drops c (eviction / departure). Guarded to preserve supply -- losing the
\* last honest holder is a push-sync/availability failure, outside pull's remit (§2).
\* A dropped holding can no longer serve, so any claim on p is released (the wire timeout).
Lose(c, p) ==
  /\ chn > 0
  /\ c \notin got                              \* churn after the pivot stored c is moot here
  /\ c \in holds[p]
  /\ \E q \in Honest \ {p} : c \in holds[q]    \* supply survives the loss
  /\ chn'   = chn - 1
  /\ holds' = [holds EXCEPT ![p] = @ \ {c}]
  /\ want'  = [want  EXCEPT ![c] = @ \ {p}]
  /\ UNCHANGED <<got, failed, arrived, conflict, tmo, ndeliv>>

\* churn: p (re)acquires c -- a returning peer, or a neighbour that completed its own fetch.
Gain(c, p) ==
  /\ chn > 0
  /\ c \notin got
  /\ c \notin holds[p]
  /\ chn'   = chn - 1
  /\ holds' = [holds EXCEPT ![p] = @ \cup {c}]
  /\ UNCHANGED <<got, want, failed, arrived, conflict, tmo, ndeliv>>

\* a LIVE chunk arrives after the historical cutoff (becomes available at its holders)
NewChunk(c) ==
  /\ c \in LiveChunks
  /\ c \notin arrived
  /\ arrived' = arrived \cup {c}
  /\ UNCHANGED <<got, want, failed, conflict, holds, tmo, chn, ndeliv>>

Next ==
  \/ \E c \in Chunks, p \in Peers : Want(c, p)
  \/ \E c \in Chunks, p \in Peers : Deliver(c, p)
  \/ \E c \in Chunks, p \in Peers : ByzStall(c, p)
  \/ \E c \in Chunks, p \in Peers : SpuriousTimeout(c, p)
  \/ \E c \in Chunks, p \in Peers : Lose(c, p)
  \/ \E c \in Chunks, p \in Peers : Gain(c, p)
  \/ \E c \in Chunks : ResetExcluded(c)
  \/ \E c \in Chunks : NewChunk(c)

\* Weak fairness on the protocol's own steps (it does retry; honest peers do answer; the
\* cooldown does expire; LIVE arrivals do land). NO fairness on SpuriousTimeout / Lose /
\* Gain: misfires and churn MAY happen (up to their budgets), they are never obligated.
Fairness ==
  /\ \A c \in Chunks, p \in Peers : WF_vars(Want(c, p))
  /\ \A c \in Chunks, p \in Peers : WF_vars(Deliver(c, p))
  /\ \A c \in Chunks, p \in Peers : WF_vars(ByzStall(c, p))
  /\ \A c \in Chunks : WF_vars(ResetExcluded(c))
  /\ \A c \in Chunks : WF_vars(NewChunk(c))         \* LIVE arrivals do eventually happen

Spec == Init /\ [][Next]_vars /\ Fairness

-----------------------------------------------------------------------------
\* Properties (correctness only; quantitative objectives are analytic, see the design doc)

Phi == Cardinality(arrived \ got)            \* deficit over what is currently available

\* --- safety (invariants) ---
ConflictFree     == conflict = FALSE         \* exactly-once delivery (spans HIST + LIVE)
DeliveryFloor    == ndeliv = Cardinality(got)            \* the O3 floor, stated directly
SupplyInv        == \A c \in Chunks : \E p \in Honest : c \in holds[p]
DedupInv         == Dedup => \A c \in Chunks : Cardinality(want[c]) <= 1
ClaimsLive       == \A c \in Chunks : \A p \in want[c] :
                        c \in holds[p] /\ p \notin failed[c]
NoFalseExclusion == TimeoutBudget = 0 =>
                        \A c \in Chunks : failed[c] \subseteq Byzantine

\* --- liveness (temporal) ---
Completeness == <>(got = Chunks)             \* every chunk fetched (LIVE arrives then is fetched)
Freshness    == <>(LiveChunks \subseteq got) \* every post-cutoff arrival eventually delivered
Quiescence   == <>[](\A c \in Chunks : want[c] = {})  \* no claim leaks past completion
Monotone     == [][got \subseteq got']_vars  \* the store never regresses
=============================================================================
