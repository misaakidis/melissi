//! Browser WASM: 26-node pull-sync for Concordance — `melissi-sim` only,
//! no wire / crypto / net / overlay.

use melissi_sim::Sim;
use melissi_types::Triple;
use std::collections::HashMap;
use wasm_bindgen::prelude::*;

const K: usize = 26;
const SEED: u64 = 0xC0FFEE;
const RING_FANOUT: usize = 2; // 4 peers: 2 on each side of the letter ring

fn triple(id: u32) -> Triple {
    Triple::mock(id)
}

#[wasm_bindgen]
pub struct Reserve {
    sim: Sim,
    texts: Vec<String>,
    origins: Vec<u8>,
    text_index: HashMap<String, u32>,
    held: Vec<Vec<bool>>,
    events: Vec<String>,
    deliveries: u32,
    converged: bool,
}

#[wasm_bindgen]
impl Reserve {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        let mut r = Reserve {
            sim: Sim::new(K, &[], SEED),
            texts: Vec::new(),
            origins: Vec::new(),
            text_index: HashMap::new(),
            held: vec![Vec::new(); K],
            events: Vec::new(),
            deliveries: 0,
            converged: false,
        };
        r.sim.start_ring(RING_FANOUT);
        r
    }

    /// Ingest a word. Returns `(word_id << 1) | is_new` as i32, or -1 if empty.
    pub fn ingest(&mut self, text: &str) -> i32 {
        let w = text.trim().to_lowercase();
        if w.is_empty() {
            return -1;
        }
        let path: Vec<u8> = w
            .bytes()
            .filter_map(|b| {
                let c = b.wrapping_sub(b'a');
                if c < 26 {
                    Some(c)
                } else {
                    None
                }
            })
            .collect();
        if path.is_empty() {
            return -1;
        }
        let origin = path[0];

        if let Some(&id) = self.text_index.get(&w) {
            self.events.push(format!("recur:{id}:{origin}"));
            return (id << 1) as i32;
        }

        let id = self.texts.len() as u32;
        self.texts.push(w.clone());
        self.origins.push(origin);
        self.text_index.insert(w, id);
        for node in &mut self.held {
            node.push(false);
        }

        let t = triple(id);
        self.sim.arrive(origin as usize, t);
        self.held[origin as usize][id as usize] = true;
        self.converged = false;
        self.events.push(format!("arrive:{id}:{origin}"));
        (id << 1 | 1) as i32
    }

    /// Run up to `budget` sim steps; emit delivery diffs as events.
    pub fn step(&mut self, budget: u32) -> u32 {
        let mut n = 0u32;
        while n < budget {
            if !self.sim.step() {
                break;
            }
            n += 1;
            self.scan_holds();
        }
        if !self.converged && !self.texts.is_empty() && self.sim_deficit_zero() {
            self.converged = true;
            self.events.push("converged".into());
        }
        n
    }

    fn sim_deficit_zero(&self) -> bool {
        (0..K).all(|i| self.sim.deficit(i) == 0)
    }

    fn scan_holds(&mut self) {
        let nwords = self.texts.len();
        if nwords == 0 {
            return;
        }
        for node in 0..K {
            for id in 0..nwords {
                if self.held[node][id] {
                    continue;
                }
                if self.sim.node_has(node, triple(id as u32)) {
                    self.held[node][id] = true;
                    self.deliveries += 1;
                    let from = self.nearest_holder(id as u32, node as u8);
                    self.events.push(format!("deliver:{id}:{from}:{node}"));
                }
            }
        }
    }

    fn nearest_holder(&self, id: u32, to: u8) -> u8 {
        let mut best = 0u8;
        let mut bd = u8::MAX;
        for h in 0..K {
            let h = h as u8;
            if h == to || !self.held[h as usize][id as usize] {
                continue;
            }
            let d = ring_dist(h, to);
            if d < bd {
                bd = d;
                best = h;
            }
        }
        best
    }

    pub fn drain_events(&mut self) -> String {
        if self.events.is_empty() {
            return String::new();
        }
        let out = self.events.join("\n");
        self.events.clear();
        out
    }

    pub fn word_count(&self) -> u32 {
        self.texts.len() as u32
    }

    pub fn held_count(&self, word_id: u32) -> u32 {
        let id = word_id as usize;
        if id >= self.texts.len() {
            return 0;
        }
        self.held.iter().filter(|row| row[id]).count() as u32
    }

    pub fn reserve_frac(&self, node: u32) -> f64 {
        let n = node as usize;
        if n >= K || self.texts.is_empty() {
            return 0.0;
        }
        let held = self.held[n].iter().filter(|&&h| h).count();
        held as f64 / self.texts.len() as f64
    }

    pub fn has_origin(&self, node: u32) -> bool {
        let n = node as u8;
        self.origins.iter().any(|&o| o == n)
    }

    pub fn progress(&self) -> f64 {
        let nw = self.texts.len();
        if nw == 0 {
            return 0.0;
        }
        let denom = nw * (K - 1);
        self.deliveries as f64 / denom as f64
    }

    pub fn is_converged(&self) -> bool {
        self.converged
    }
}

fn ring_dist(a: u8, b: u8) -> u8 {
    let d = a.abs_diff(b);
    d.min((K as u8).wrapping_sub(d))
}
