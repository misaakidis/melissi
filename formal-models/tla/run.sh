#!/usr/bin/env bash
# SWIP-25E testbed. Green ONLY IF every positive config passes AND every ablation
# produces its expected counterexample (each mechanism shown load-bearing).
# -deadlock: terminal (reserve-filled) states are not deadlocks here; liveness does the work.
#
# Ablation attribution: TLC names a violated INVARIANT but not a violated temporal
# property — so every ablation cfg lists EXACTLY ONE temporal property (the expected one),
# and neg() accepts a temporal violation only under that discipline. A violation of any
# OTHER property (e.g. a safety invariant breaking in a liveness ablation) is a FAIL.
set -u
cd "$(dirname "$0")"
JAR="${TLA2TOOLS:-$HOME/tla2tools.jar}"
mkdir -p counterexamples
rc=0
run() { rm -rf "states/$1"; java -cp "$JAR" tlc2.TLC -deadlock -workers auto -metadir "states/$1" -config "$1.cfg" "$1.tla" 2>&1; }
trace() { echo "$2" | sed -n '/violated/,$p' | sed '/^Finished in /q' | head -120 > "counterexamples/$1.txt" 2>/dev/null || true; }

pos() {
  out=$(run "$1")
  if echo "$out" | grep -q "No error has been found"; then
    echo "PASS  $1   ($(echo "$out" | grep -oE '[0-9,]+ distinct states' | tail -1))"
  else
    echo "FAIL  $1   (expected pass)"; echo "$out" | tail -8; rc=1
  fi
}

neg() {  # $1 config, $2 expected-violated property name
  out=$(run "$1")
  if echo "$out" | grep -q "Invariant $2 is violated"; then
    echo "PASS  $1   (ablation: invariant $2 violated as expected)"
    trace "$1" "$out"
  elif echo "$out" | grep -q "Temporal properties were violated"; then
    # TLC does not name the temporal property; attribution holds because the cfg
    # checks exactly one — verify that discipline rather than trusting it.
    cfgprops=$(sed -n 's/^PROPERTIES *//p' "$1.cfg" | xargs)
    if [ "$cfgprops" = "$2" ]; then
      echo "PASS  $1   (ablation: temporal $2 violated as expected — sole temporal property in cfg)"
      trace "$1" "$out"
    else
      echo "FAIL  $1   (temporal violation, but cfg checks '$cfgprops' — cannot attribute to $2)"; rc=1
    fi
  else
    echo "FAIL  $1   (expected $2 violation, got none)"; echo "$out" | tail -8; rc=1
  fi
}

echo "== positives =="
pos MC_base
pos MC_partial
pos MC_omission
pos MC_vicinity
pos MC_live
pos MC_atomic
echo "== positives: relaxed assumptions (idealisations as knobs) =="
pos MC_timeout            # spurious timeouts on honest peers + reset-on-exhaustion
pos MC_churn              # bounded holdings churn under an omission adversary
pos MC_storm              # composite: Byz + LIVE + priority + timeout + churn, k=4
pos MC_scale              # k=6, two Byzantine omitters
echo "== interval settlement (resume layer; companion IntervalSettlement.tla) =="
pos MC_settlement         # settled-only advance: NoDrop + completeness + intervals drain
echo "== offer pacing (advertisement round; companion OfferPacing.tla) =="
pos MC_pacing             # adverts = justifications (AdvertBound) and coverage still drains
pos MC_pacing_griefed     # §6.2: instantly-empty answers cannot extort adverts beyond the floor
echo "== neighbourhood (discovery logic; companion Neighbourhood.tla) =="
pos MC_nhood              # converges: every bin to K, neighbourhood dense, working set bounded
echo "== no-barrier windowed scheduling (companions DiscoveryBarrier.tla / WindowedLoad.tla) =="
pos MC_barrier_off        # no barrier: honest holders scheduled despite a withholder, with NO timeout
pos MC_barrier            # barrier + per-lister timeout drains (a scarce budget would not -- misattribution)
pos MC_windowed           # shipped scheduler, all-together offers (small lag): serve-load within 1
echo "== ablations =="
neg MC_nodedup    ConflictFree
neg MC_nofailover Completeness
neg MC_noexclude  Completeness
neg MC_single_omission Completeness
neg MC_single_partial  Completeness
neg MC_no_live    Freshness
neg MC_nonatomic  ConflictFree
neg MC_noreset    Completeness     # permanent exclusion + one misfire -> chunk unfetchable
neg MC_settlement_eager    NoDrop           # eager (master) advance -> an unfetched chunk is forgotten
neg MC_settlement_noreject AdvanceComplete  # rejections don't settle -> resume bookkeeping wedges
neg MC_respawn             AdvertBound      # unpaced re-advert -> the empty-offer respawn busy-loop
neg MC_nhood_flat    NeighbourhoodComplete  # flat "K everywhere" -> neighbourhood never densely connects
neg MC_nhood_noprune Bounded                # no shedding -> working set never contracts as depth rises
neg MC_barrier_wedge Progress      # barrier without timeout -> one withholder wedges the whole bin
neg MC_staggered     SkewBound     # staggered discovery -> a late offerer starts behind, skew tracks the head-start
echo
[ $rc -eq 0 ] && echo "ALL GREEN (positives pass; ablations fail as expected)" || echo "SUITE RED"
exit $rc
