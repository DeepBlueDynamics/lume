//! Live search-dynamics stream (`lume stream`).
//!
//! Runs a phase-binding + Weber relaxation over one or more queries and the
//! union of their top-K retrieved candidates, emitting one NDJSON frame per
//! simulation step to stdout. Each frame carries, per node: a 3D PCA projection
//! of its (warped) 768-D vector, its 3D velocity and acceleration, its Kuramoto
//! phase, and — the thing static retrieval throws away — its approach
//! acceleration toward its anchor query (`d̈` of the cosine distance).
//!
//! Multiple queries (additive search) share one PCA frame and one relaxation, so
//! candidates retrieved by more than one query (`members.len() > 1`) are genuine
//! overlaps in the same space. A browser bridge relays frames to a React/three.js
//! view. Read-only against shivvr (embeddings only); the dynamics mirror
//! `gte_weber_teacher.run_teacher` from the psyche research line.

use std::collections::HashMap;

use crate::bm25::Bm25Index;
use crate::hybrid::{embed_text, load_nuts_token};
use crate::semantic_mesh::SimpleRng;

const DT: f64 = 0.05;
const EPS: f64 = 1e-12;

/// Tuning knobs for the relaxation, mirroring the Weber teacher defaults.
pub struct StreamParams {
    pub steps: usize,
    pub candidates: usize,
    pub beta_warp: f64,
    pub k0: f64,
    pub noise: f64,
    pub c_weber: f64,
    pub sigma_v: f64,
}

impl Default for StreamParams {
    fn default() -> Self {
        Self { steps: 160, candidates: 24, beta_warp: 0.02, k0: 1.6, noise: 0.04, c_weber: 1.5, sigma_v: 1.2 }
    }
}

/// A retrieved candidate and which query indices surfaced it (its overlap set).
pub struct Candidate {
    pub section_id: usize,
    pub score: f64,
    pub members: Vec<usize>,
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}
fn norm(v: &[f64]) -> f64 {
    dot(v, v).max(EPS).sqrt()
}
fn normalize(v: &[f64]) -> Vec<f64> {
    let n = norm(v);
    v.iter().map(|x| x / n).collect()
}
fn cosine(a: &[f64], b: &[f64]) -> f64 {
    dot(a, b) / (norm(a) * norm(b))
}
fn distance(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum::<f64>().sqrt()
}
fn wrap_phase(a: f64) -> f64 {
    a - 2.0 * std::f64::consts::PI * ((a + std::f64::consts::PI) / (2.0 * std::f64::consts::PI)).floor()
}

/// A concept node: its 768-D vector plus oscillator state.
struct Node {
    label: String,
    score: f64,
    text: String,
    jitter: [f64; 3],
    v: Vec<f64>,
    theta: f64,
    omega: f64,
    is_query: bool,
    query_index: usize,   // which query (for query nodes)
    members: Vec<usize>,  // query indices that retrieved this candidate
    anchor: usize,        // node index of the candidate's nearest query
    pos: [f64; 3],
    vel: [f64; 3],
    acc: [f64; 3],
    dq: f64,
    approach_vel: f64,
    approach_acc: f64,
}

/// Top-3 PCA basis fitted once over the initial vectors (centered).
struct Pca {
    mean: Vec<f64>,
    axes: [Vec<f64>; 3],
}

impl Pca {
    fn fit(vectors: &[Vec<f64>], rng: &mut SimpleRng) -> Pca {
        let d = vectors[0].len();
        let n = vectors.len() as f64;
        let mut mean = vec![0.0; d];
        for v in vectors {
            for (m, x) in mean.iter_mut().zip(v) {
                *m += x / n;
            }
        }
        let centered: Vec<Vec<f64>> = vectors
            .iter()
            .map(|v| v.iter().zip(&mean).map(|(x, m)| x - m).collect())
            .collect();

        let mut axes: Vec<Vec<f64>> = Vec::new();
        for _ in 0..3 {
            let mut vec: Vec<f64> = (0..d).map(|_| rng.next_u64() as f64 / u64::MAX as f64 - 0.5).collect();
            for _ in 0..40 {
                let mut next = vec![0.0; d];
                for row in &centered {
                    let proj = dot(row, &vec);
                    for (nx, rx) in next.iter_mut().zip(row) {
                        *nx += proj * rx;
                    }
                }
                for prev in &axes {
                    let p = dot(&next, prev);
                    for (nx, px) in next.iter_mut().zip(prev) {
                        *nx -= p * px;
                    }
                }
                let nn = norm(&next);
                if nn < EPS {
                    break;
                }
                vec = next.iter().map(|x| x / nn).collect();
            }
            axes.push(vec);
        }
        Pca { mean, axes: [axes[0].clone(), axes[1].clone(), axes[2].clone()] }
    }

    fn project(&self, v: &[f64]) -> [f64; 3] {
        let c: Vec<f64> = v.iter().zip(&self.mean).map(|(x, m)| x - m).collect();
        [dot(&c, &self.axes[0]), dot(&c, &self.axes[1]), dot(&c, &self.axes[2])]
    }
}

fn emit(line: &serde_json::Value) {
    println!("{}", line);
}

/// Runs the relaxation for `queries` over the union `cands` and streams frames.
pub fn run(
    bm25: &Bm25Index,
    queries: &[String],
    cands: &[Candidate],
    params: &StreamParams,
) -> Result<(), String> {
    let token = load_nuts_token().ok_or_else(|| {
        "shivvr token unavailable (need a local shivvr endpoint or NUTS_SERVICES_TOKEN)".to_string()
    })?;

    eprintln!("[stream] embedding {} queries + {} candidates via shivvr…", queries.len(), cands.len());
    let mut rng = SimpleRng::new();
    let mut nodes: Vec<Node> = Vec::new();

    // Query anchor nodes (fixed). One per query.
    for (qi, q) in queries.iter().enumerate() {
        let qv = normalize(&embed_text(q, &token)?);
        nodes.push(Node {
            label: format!("◆ {}", truncate(q, 42)),
            score: 0.0,
            text: q.clone(),
            jitter: [0.0, 0.0, 0.0],
            v: qv,
            theta: 0.0,
            omega: 0.0,
            is_query: true,
            query_index: qi,
            members: vec![qi],
            anchor: qi,
            pos: [0.0; 3], vel: [0.0; 3], acc: [0.0; 3],
            dq: 0.0, approach_vel: 0.0, approach_acc: 0.0,
        });
    }
    let nq = nodes.len();
    if nq == 0 {
        return Err("no queries given".to_string());
    }

    // Candidate nodes (warped). Anchor = nearest query.
    for c in cands {
        let sec = match bm25.sections.get(c.section_id) {
            Some(s) => s,
            None => continue,
        };
        let snippet: String = sec.body.split_whitespace().take(80).collect::<Vec<_>>().join(" ");
        let v = match embed_text(&snippet, &token) {
            Ok(v) => normalize(&v),
            Err(_) => continue,
        };
        // Nearest query among this candidate's members (fall back to all queries).
        let pool: Vec<usize> = if c.members.is_empty() { (0..nq).collect() } else { c.members.clone() };
        let anchor = pool
            .iter()
            .copied()
            .max_by(|&a, &b| cosine(&v, &nodes[a].v).partial_cmp(&cosine(&v, &nodes[b].v)).unwrap())
            .unwrap_or(0);
        let text: String = sec.body.split_whitespace().take(70).collect::<Vec<_>>().join(" ");
        nodes.push(Node {
            label: section_label(sec, c.section_id),
            score: c.score,
            text: truncate(&text, 320),
            jitter: jitter_for(c.section_id),
            v,
            theta: (rng.next_u64() as f64 / u64::MAX as f64) * 2.0 * std::f64::consts::PI - std::f64::consts::PI,
            omega: (rng.next_u64() as f64 / u64::MAX as f64 - 0.5) * 0.1,
            is_query: false,
            query_index: anchor,
            members: c.members.clone(),
            anchor,
            pos: [0.0; 3], vel: [0.0; 3], acc: [0.0; 3],
            dq: 0.0, approach_vel: 0.0, approach_acc: 0.0,
        });
    }
    if nodes.len() <= nq {
        return Err("not enough embeddable candidates to run the dynamics".to_string());
    }
    let m = nodes.len();

    // Anchor vectors are fixed (query nodes never warp), so snapshot them.
    let anchor_v: Vec<Vec<f64>> = nodes.iter().map(|n| nodes[n.anchor].v.clone()).collect();

    // PCA frame, centered on the query centroid so the field sits among the
    // anchors; scaled so the median anchor-distance is ~1 unit of a few.
    let pca = Pca::fit(&nodes.iter().map(|n| n.v.clone()).collect::<Vec<_>>(), &mut rng);
    let mut center = [0.0f64; 3];
    for n in nodes.iter().take(nq) {
        let p = pca.project(&n.v);
        center[0] += p[0] / nq as f64;
        center[1] += p[1] / nq as f64;
        center[2] += p[2] / nq as f64;
    }
    let mut scale = 1.0;
    {
        let mut ds: Vec<f64> = nodes.iter().skip(nq).map(|n| {
            let p = pca.project(&n.v);
            ((p[0] - center[0]).powi(2) + (p[1] - center[1]).powi(2) + (p[2] - center[2]).powi(2)).sqrt()
        }).filter(|d| *d > EPS).collect();
        ds.sort_by(|a, b| a.partial_cmp(b).unwrap());
        if let Some(med) = ds.get(ds.len() / 2) {
            if *med > EPS { scale = 5.5 / med; }
        }
    }
    let project = |pca: &Pca, v: &[f64]| {
        let p = pca.project(v);
        [(p[0] - center[0]) * scale, (p[1] - center[1]) * scale, (p[2] - center[2]) * scale]
    };

    // Query anchors are laid out by PCA so multiple queries separate in space.
    // Each candidate then orbits its anchor query on a fixed ray, at a radius
    // that shrinks as it becomes more similar to that query — so results cluster
    // AROUND their query (nearest = most relevant), and binding visibly pulls
    // them inward, which makes the acceleration arrows read as true approach.
    let qpos: Vec<[f64; 3]> = (0..nq).map(|i| project(&pca, &nodes[i].v)).collect();
    let radius = |cos: f64| 0.7 + 3.0 * (1.0 - cos).max(0.0);
    let mut dirs: Vec<[f64; 3]> = vec![[0.0, 1.0, 0.0]; m];
    for i in nq..m {
        let p = project(&pca, &nodes[i].v);
        let a = qpos[nodes[i].anchor];
        let mut d = [p[0] - a[0], p[1] - a[1], p[2] - a[2]];
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        if len < 1e-6 {
            d = nodes[i].jitter; // degenerate (candidate ≈ query): deterministic ray
        }
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-6);
        dirs[i] = [d[0] / len, d[1] / len, d[2] / len];
    }

    for i in 0..m {
        nodes[i].dq = 1.0 - cosine(&nodes[i].v, &anchor_v[i]);
        nodes[i].pos = if nodes[i].is_query {
            qpos[nodes[i].query_index]
        } else {
            let r = radius(1.0 - nodes[i].dq);
            let a = qpos[nodes[i].anchor];
            [a[0] + dirs[i][0] * r, a[1] + dirs[i][1] * r, a[2] + dirs[i][2] * r]
        };
    }

    emit(&serde_json::json!({
        "type": "meta",
        "queries": queries,
        "steps": params.steps,
        "nodes": nodes.iter().enumerate().map(|(i, n)| serde_json::json!({
            "id": i, "label": n.label, "score": n.score, "text": n.text,
            "is_query": n.is_query, "query_index": n.query_index, "members": n.members,
        })).collect::<Vec<_>>(),
    }));

    let mut prev_d = vec![vec![0.0f64; m]; m];
    let mut prev2_d = vec![vec![0.0f64; m]; m];
    for i in 0..m {
        for j in 0..m {
            if i != j {
                let d = distance(&nodes[i].v, &nodes[j].v);
                prev_d[i][j] = d;
                prev2_d[i][j] = d;
            }
        }
    }
    let mut weights: HashMap<(usize, usize), f64> = HashMap::new();
    for i in 0..m {
        for j in 0..m {
            if i != j {
                weights.insert((i, j), 0.2 + cosine(&nodes[i].v, &nodes[j].v).max(0.0));
            }
        }
    }

    for step in 0..params.steps {
        let mut couplings = vec![vec![0.0f64; m]; m];
        let snapshot: Vec<Vec<f64>> = nodes.iter().map(|n| n.v.clone()).collect();

        for i in 0..m {
            if nodes[i].is_query {
                continue;
            }
            let mut vi = nodes[i].v.clone();
            for j in 0..m {
                if i == j {
                    continue;
                }
                let d = distance(&vi, &snapshot[j]);
                let rdot = (d - prev_d[i][j]) / DT;
                let rdot_prev = (prev_d[i][j] - prev2_d[i][j]) / DT;
                let rddot = (rdot - rdot_prev) / DT;
                prev2_d[i][j] = prev_d[i][j];
                prev_d[i][j] = d;

                let term1 = (rdot * rdot) / (2.0 * params.c_weber * params.c_weber);
                let term2 = (d * rddot) / (params.c_weber * params.c_weber);
                let b_ij = (1.0 - term1 + term2).clamp(-2.0, 2.0);

                let align = (nodes[j].theta - nodes[i].theta).cos();
                let w = weights.get_mut(&(i, j)).unwrap();
                *w += 0.1 * (align - *w) * DT;
                *w = w.clamp(0.05, 5.0);
                let wij = *w;

                let delta: Vec<f64> = snapshot[j].iter().zip(&vi).map(|(x, y)| x - y).collect();
                if align > 0.5 {
                    vi = normalize(&vi.iter().zip(&delta).map(|(x, dx)| x + params.beta_warp * DT * dx).collect::<Vec<_>>());
                } else if align < -0.5 {
                    vi = normalize(&vi.iter().zip(&delta).map(|(x, dx)| x - params.beta_warp * DT * dx).collect::<Vec<_>>());
                }

                let sem = distance(&vi, &snapshot[j]);
                let s_ij = (-(sem * sem) / (params.sigma_v * params.sigma_v)).exp();
                couplings[i][j] = params.k0 * wij * s_ij * b_ij;
            }
            nodes[i].v = vi;
        }

        let thetas: Vec<f64> = nodes.iter().map(|n| n.theta).collect();
        let mut next_theta = thetas.clone();
        for i in 0..m {
            if nodes[i].is_query {
                continue;
            }
            let mut torque = 0.0;
            for j in 0..m {
                if i != j {
                    torque += couplings[i][j] * (thetas[j] - thetas[i]).sin();
                }
            }
            let xi = (rng.next_u64() as f64 / u64::MAX as f64 - 0.5) * 2.0;
            next_theta[i] = thetas[i] + (nodes[i].omega + torque) * DT + params.noise * DT.sqrt() * xi;
        }
        for (n, t) in nodes.iter_mut().zip(next_theta) {
            n.theta = wrap_phase(t);
        }

        for i in 0..m {
            let dq = 1.0 - cosine(&nodes[i].v, &anchor_v[i]);
            let new_pos = if nodes[i].is_query {
                qpos[nodes[i].query_index]
            } else {
                let r = radius(1.0 - dq);
                let a = qpos[nodes[i].anchor];
                [a[0] + dirs[i][0] * r, a[1] + dirs[i][1] * r, a[2] + dirs[i][2] * r]
            };
            let new_vel = [new_pos[0] - nodes[i].pos[0], new_pos[1] - nodes[i].pos[1], new_pos[2] - nodes[i].pos[2]];
            nodes[i].acc = [new_vel[0] - nodes[i].vel[0], new_vel[1] - nodes[i].vel[1], new_vel[2] - nodes[i].vel[2]];
            nodes[i].vel = new_vel;
            nodes[i].pos = new_pos;

            let nav = (dq - nodes[i].dq) / DT;
            nodes[i].approach_acc = (nav - nodes[i].approach_vel) / DT;
            nodes[i].approach_vel = nav;
            nodes[i].dq = dq;
        }

        let (mut cs, mut sn, mut cnt) = (0.0, 0.0, 0.0);
        for n in nodes.iter().filter(|n| !n.is_query) {
            cs += n.theta.cos();
            sn += n.theta.sin();
            cnt += 1.0;
        }
        let r_global = if cnt > 0.0 { ((cs / cnt).powi(2) + (sn / cnt).powi(2)).sqrt() } else { 0.0 };
        let clusters = cluster_ids(&nodes);

        let frame = serde_json::json!({
            "type": "frame",
            "step": step,
            "r_global": r_global,
            "nodes": nodes.iter().enumerate().map(|(i, n)| serde_json::json!({
                "id": i,
                "pos": [n.pos[0] + n.jitter[0], n.pos[1] + n.jitter[1], n.pos[2] + n.jitter[2]],
                "vel": n.vel,
                "acc": n.acc,
                "phase": n.theta,
                "cos_q": cosine(&n.v, &anchor_v[i]),
                "approach_vel": n.approach_vel,
                "approach_acc": n.approach_acc,
                "cluster": clusters[i],
                "is_query": n.is_query,
                "query_index": n.query_index,
                "members": n.members,
            })).collect::<Vec<_>>(),
        });
        emit(&frame);
    }

    emit(&serde_json::json!({ "type": "done" }));
    Ok(())
}

/// Union-find clustering on the current warped cosine (> 0.62).
fn cluster_ids(nodes: &[Node]) -> Vec<usize> {
    let m = nodes.len();
    let mut parent: Vec<usize> = (0..m).collect();
    fn find(p: &mut Vec<usize>, x: usize) -> usize {
        let mut r = x;
        while p[r] != r {
            r = p[r];
        }
        p[x] = r;
        r
    }
    for i in 0..m {
        for j in (i + 1)..m {
            if cosine(&nodes[i].v, &nodes[j].v) > 0.62 {
                let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                if ri != rj {
                    parent[ri] = rj;
                }
            }
        }
    }
    let mut remap: HashMap<usize, usize> = HashMap::new();
    (0..m).map(|i| {
        let r = find(&mut parent, i);
        let next = remap.len();
        *remap.entry(r).or_insert(next)
    }).collect()
}

/// Deterministic small 3D offset per section id so near-identical vectors don't
/// render exactly on top of each other.
fn jitter_for(sid: usize) -> [f64; 3] {
    let mut h = (sid as u64).wrapping_mul(0x9E3779B97F4A7C15).wrapping_add(0x123456789);
    let mut comp = || {
        h ^= h >> 33;
        h = h.wrapping_mul(0xFF51AFD7ED558CCD);
        h ^= h >> 33;
        ((h as f64 / u64::MAX as f64) - 0.5) * 0.5
    };
    [comp(), comp(), comp()]
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max - 1).collect::<String>())
    }
}

fn section_label(sec: &crate::bm25::Section, sid: usize) -> String {
    let title = sec.title.trim();
    if !title.is_empty() {
        truncate(title, 48)
    } else {
        let head: String = sec.body.split_whitespace().take(6).collect::<Vec<_>>().join(" ");
        format!("§{} {}", sid, truncate(&head, 40))
    }
}
