------------------------- MODULE MC_compose_single -------------------------
\* Ablation: ConnectAll OFF — single-source. The node connects only its seed a,
\* which holds {1,2} but not 3. No neighbour holds the whole reserve, so chunk 3
\* (held by honest b) is never fetched. Expect: Completeness violated — the
\* single-source dependency §5.1 removes, shown end to end.
EXTENDS Naturals, FiniteSets, TLC
Chunks == {1, 2, 3}
Peers  == {"a", "b", "c"}
Honest == {"a", "b"}
Holds  == ("a" :> {1, 2}) @@ ("b" :> {2, 3}) @@ ("c" :> {1, 2, 3})
Seed   == "a"
ConnectAll == FALSE
VARIABLES conn, got
INSTANCE Composition
============================================================================
