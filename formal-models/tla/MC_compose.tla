---------------------------- MODULE MC_compose ----------------------------
\* Composition, positive: 3 honest-covered reserve chunks; holdings SPREAD across
\* honest a={1,2} and b={2,3}, with a Byzantine c={1,2,3} that serves nothing;
\* bootstrap from a. Expect: connecting the whole honest neighbourhood assembles
\* the supply and the reserve completes (chunk 3 fetched from b, c ignored).
EXTENDS Naturals, FiniteSets, TLC
Chunks == {1, 2, 3}
Peers  == {"a", "b", "c"}
Honest == {"a", "b"}
Holds  == ("a" :> {1, 2}) @@ ("b" :> {2, 3}) @@ ("c" :> {1, 2, 3})
Seed   == "a"
ConnectAll == TRUE
VARIABLES conn, got
INSTANCE Composition
===========================================================================
