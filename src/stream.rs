//! Live search-dynamics stream (`lume stream`).
//!
//! Runs a phase-binding + Weber relaxation over a query and its top-K retrieved
//! candidates and emits one NDJSON frame per simulation step to stdout. Each
//! frame carries, per node: a 3D PCA projection of its (warped) 768-D vector,
//! its 3D velocity and acceleration in that frame, its Kuramoto phase, and —
//! the thing static retrieval throws away — its **approach acceleration toward
//! the query** (`d̈` of the cosine distance). A browser bridge relays these
//! frames to a React/three.js view so you can watch candidates accelerate into
//! (or veer away from) the query as the dynamics settle.
//!
//! This is a visualization/re-ranking probe, not the production search path. It
//! reuses the same shivvr embeddings and the same SKG candidate set as `lume
//! search`; the dynamics mirror `gte_weber_teacher.run_teacher` from the psyche
//! research line (Kuramoto phase coupling × Weber relational modulation × in-
//! phase vector warping).

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
    v: Vec<f64>,
    theta: f64,
    omega: f64,
    is_query: bool,
    // 3D kinematics tracked across steps in the fixed PCA frame.
    pos: [f64; 3],
    vel: [f64; 3],
    acc: [f64; 3],
    // approach-to-query kinematics (cosine distance to the query node).
    dq: f64,
    approach_vel: f64,
    approach_acc: f64,
}

/// Top-3 PCA basis fitted once over the initial vectors (centered). Power
/// iteration on the Gram matrix `XᵀX` with deflation — small N, pure std.
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
        // Centered rows.
        let centered: Vec<Vec<f64>> = vectors
            .iter()
            .map(|v| v.iter().zip(&mean).map(|(x, m)| x - m).collect())
            .collect();

        let mut axes: Vec<Vec<f64>> = Vec::new();
        for _ in 0..3 {
            // Random init, then power-iterate v ← Σ_r r (rᵀv), orthogonalized
            // against axes already found (deflation).
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
        let centered: Vec<f64> = v.iter().zip(&self.mean).map(|(x, m)| x - m).collect();
        [dot(&centered, &self.axes[0]), dot(&centered, &self.axes[1]), dot(&centered, &self.axes[2])]
    }
}

fn emit(line: &serde_json::Value) {
    println!("{}", line);
}

/// Runs the relaxation for `query` over its top-K candidates and streams frames.
pub fn run(
    bm25: &Bm25Index,
    candidate_hits: &[(usize, f64)], // (section_index, lexical/skg score), already ranked
    query: &str,
    params: &StreamParams,
) -> Result<(), String> {
    let token = load_nuts_token().ok_or_else(|| {
        "shivvr token unavailable (need a local shivvr endpoint or NUTS_SERVICES_TOKEN)".to_string()
    })?;

    // Embed the query and each candidate section (shivvr 768-D GTR-T5).
    eprintln!("[stream] embedding query + {} candidates via shivvr…", candidate_hits.len());
    let qv = normalize(&embed_text(query, &token)?);

    let mut nodes: Vec<Node> = Vec::new();
    nodes.push(Node {
        label: format!("◆ {}", truncate(query, 48)),
        v: qv.clone(),
        theta: 0.0,
        omega: 0.0,
        is_query: true,
        pos: [0.0; 3],
        vel: [0.0; 3],
        acc: [0.0; 3],
        dq: 0.0,
        approach_vel: 0.0,
        approach_acc: 0.0,
    });

    let mut rng = SimpleRng::new();
    for (sid, _score) in candidate_hits {
        let sec = match bm25.sections.get(*sid) {
            Some(s) => s,
            None => continue,
        };
        let snippet: String = sec.body.split_whitespace().take(80).collect::<Vec<_>>().join(" ");
        let v = match embed_text(&snippet, &token) {
            Ok(v) => normalize(&v),
            Err(_) => continue,
        };
        let label = section_label(sec, *sid);
        nodes.push(Node {
            label,
            v,
            theta: (rng.next_u64() as f64 / u64::MAX as f64) * 2.0 * std::f64::consts::PI - std::f64::consts::PI,
            omega: (rng.next_u64() as f64 / u64::MAX as f64 - 0.5) * 0.1,
            is_query: false,
            pos: [0.0; 3],
            vel: [0.0; 3],
            acc: [0.0; 3],
            dq: 0.0,
            approach_vel: 0.0,
            approach_acc: 0.0,
        });
    }
    if nodes.len() < 3 {
        return Err("not enough embeddable candidates to run the dynamics".to_string());
    }

    // Fixed PCA frame from the initial geometry, so motion is comparable across
    // steps. Scale so the median query-distance is ~1 for a tidy view.
    let pca = Pca::fit(&nodes.iter().map(|n| n.v.clone()).collect::<Vec<_>>(), &mut rng);
    let q_proj = pca.project(&qv);
    let mut scale = 1.0;
    {
        let mut ds: Vec<f64> = nodes
            .iter()
            .map(|n| {
                let p = pca.project(&n.v);
                ((p[0] - q_proj[0]).powi(2) + (p[1] - q_proj[1]).powi(2) + (p[2] - q_proj[2]).powi(2)).sqrt()
            })
            .filter(|d| *d > EPS)
            .collect();
        ds.sort_by(|a, b| a.partial_cmp(b).unwrap());
        if let Some(med) = ds.get(ds.len() / 2) {
            if *med > EPS {
                scale = 3.0 / med;
            }
        }
    }
    let project = |pca: &Pca, v: &[f64]| {
        let p = pca.project(v);
        [(p[0] - q_proj[0]) * scale, (p[1] - q_proj[1]) * scale, (p[2] - q_proj[2]) * scale]
    };

    // Seed kinematics.
    for n in nodes.iter_mut() {
        n.pos = project(&pca, &n.v);
        n.vel = [0.0; 3];
        n.acc = [0.0; 3];
        n.dq = 1.0 - cosine(&n.v, &qv);
        n.approach_vel = 0.0;
        n.approach_acc = 0.0;
    }

    // Meta frame: labels + config (sent once so the client can lay out).
    emit(&serde_json::json!({
        "type": "meta",
        "query": query,
        "steps": params.steps,
        "nodes": nodes.iter().enumerate().map(|(i, n)| serde_json::json!({
            "id": i, "label": n.label, "is_query": n.is_query
        })).collect::<Vec<_>>(),
    }));

    // Pairwise distance history for the Weber term.
    let m = nodes.len();
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

        // Vector warp + Weber coupling. Snapshot vectors so updates within a
        // step read consistent neighbors.
        let snapshot: Vec<Vec<f64>> = nodes.iter().map(|n| n.v.clone()).collect();
        for i in 0..m {
            if nodes[i].is_query {
                continue; // query is a fixed anchor
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

        // Kuramoto phase update.
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

        // Update 3D kinematics + approach-to-query kinematics. Velocity is the
        // per-step position delta; acceleration is the change in velocity — the
        // thing static layouts never show.
        for n in nodes.iter_mut() {
            let new_pos = project(&pca, &n.v);
            let new_vel = [new_pos[0] - n.pos[0], new_pos[1] - n.pos[1], new_pos[2] - n.pos[2]];
            n.acc = [new_vel[0] - n.vel[0], new_vel[1] - n.vel[1], new_vel[2] - n.vel[2]];
            n.vel = new_vel;
            n.pos = new_pos;

            let dq = 1.0 - cosine(&n.v, &qv);
            let new_approach_vel = (dq - n.dq) / DT;
            n.approach_acc = (new_approach_vel - n.approach_vel) / DT;
            n.approach_vel = new_approach_vel;
            n.dq = dq;
        }

        // Global phase coherence (Kuramoto order parameter) over candidates.
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
            "nodes": nodes.iter().enumerate().map(|(i, n)| {
                serde_json::json!({
                    "id": i,
                    "pos": n.pos,
                    "vel": n.vel,
                    "acc": n.acc,
                    "phase": n.theta,
                    "cos_q": cosine(&n.v, &qv),
                    "approach_vel": n.approach_vel,
                    "approach_acc": n.approach_acc,
                    "cluster": clusters[i],
                    "is_query": n.is_query,
                })
            }).collect::<Vec<_>>(),
        });
        emit(&frame);
    }

    emit(&serde_json::json!({ "type": "done" }));
    Ok(())
}

/// Union-find clustering on the current warped cosine (> 0.62) so the view shows
/// emergent assemblies. The query joins the cluster of its strongest binder.
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
