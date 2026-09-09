//! Top-2 principal components for the holographic projection view.
//!
//! The projection needs the two dominant eigenpairs of the centered Gram
//! operator `G = Fc·Fcᵀ` for an `n × d` feature matrix with `n ≤ 2000` and
//! `d = 4096`. This runs Lanczos with full reorthogonalization against `G`
//! applied implicitly — `G·v = Fc·(Fcᵀ·v)` with the centering folded into the
//! two products — so neither `Fc` nor the `n × n` Gram matrix is ever
//! materialized: the working set is the Krylov basis (`steps × n`) plus one
//! `d`-vector, and each step costs one pass over the features instead of the
//! `O(n²·d)` Gram build. Lanczos converges in far fewer operator applications
//! than power iteration on the flat token spectra real fact stores produce
//! (22–30 steps for 2000 facts versus 200 fixed iterations per component).

use tracedecay_store::FactReadControl;

use super::MemoryAnalysisError;

/// Krylov-dimension budget. Representative 2000-fact stores converge in
/// 22–30 steps; at this ceiling the retained basis is `96·n` values (~1.5 MB at
/// the projection point cap). Reaching it returns the best Ritz pairs so far,
/// as the fixed-iteration predecessor did.
const LANCZOS_MAX_STEPS: usize = 96;
/// A component is converged once its Ritz residual `‖G·x − θ·x‖ = β·|y_k|` is
/// within this fraction of the dominant eigenvalue. Scores are normalized to
/// `[-1, 1]` and rounded to 1e-6 at the payload, so 1e-10 leaves ample headroom.
const CONVERGENCE_TOLERANCE: f64 = 1e-10;
/// Eigenvalues (and Lanczos off-diagonals) at or below this fraction of the
/// dominant eigenvalue are a rank deficit: there is no further variance to
/// extract, so remaining components stay zero.
const RANK_FLOOR: f64 = 1e-12;
/// Lanczos steps between Ritz solves. Each solve is an `O(k³)` Jacobi pass, so
/// solving on every step would rival the operator passes once the basis is
/// large; checking every few steps costs at most that many extra steps after
/// convergence.
const RITZ_CHECK_INTERVAL: usize = 4;
/// Cyclic Jacobi sweeps allowed for the `k × k` tridiagonal Ritz problem
/// (`k ≤ LANCZOS_MAX_STEPS`); finite symmetric input converges quadratically in
/// well under this many, so hitting it is reported, not silently accepted.
const JACOBI_MAX_SWEEPS: usize = 64;

/// Top-2 principal-component scores of the centered feature matrix, scaled by
/// `sqrt(eigenvalue)` and normalized so the largest magnitude is 1. Returns
/// `Ok(None)` when fewer than two rows or an empty row leave nothing to
/// project. Callers cap `n` at `PROJECTION_POINT_CAP` and run this on the
/// blocking pool (see `memory_service::projection`).
pub fn pca_scores(
    features: &[Vec<f64>],
    read_control: &FactReadControl,
) -> Result<Option<Vec<[f64; 2]>>, MemoryAnalysisError> {
    if read_control.interrupted() {
        return Err(MemoryAnalysisError::Interrupted);
    }
    let n = features.len();
    let Some(d) = features.first().map(Vec::len) else {
        return Ok(None);
    };
    if n < 2 || d == 0 {
        return Ok(None);
    }
    let mut mean = vec![0.0; d];
    for (index, row) in features.iter().enumerate() {
        if read_control.interrupted() {
            return Err(MemoryAnalysisError::Interrupted);
        }
        if row.iter().any(|value| !value.is_finite()) {
            return Err(MemoryAnalysisError::NonFiniteFeature { index });
        }
        for (m, v) in mean.iter_mut().zip(row) {
            *m += v;
        }
    }
    for m in &mut mean {
        *m /= n as f64;
    }

    // Deterministic start direction; converged components are signed to have
    // a non-negative projection onto it, which is the sign power iteration
    // from this vector settles on.
    let mut start: Vec<f64> = (0..n).map(|i| 1.0 + (i as f64 % 7.0) / 7.0).collect();
    let start_norm = dot(&start, &start).sqrt();
    for x in &mut start {
        *x /= start_norm;
    }

    let mut basis: Vec<Vec<f64>> = vec![start.clone()];
    let mut alphas: Vec<f64> = Vec::new();
    let mut betas: Vec<f64> = Vec::new();
    let mut w = vec![0.0; n];
    let mut projected = vec![0.0; d];
    let mut ritz: Option<RitzPairs> = None;
    let max_steps = LANCZOS_MAX_STEPS.min(n);
    for step in 0..max_steps {
        if read_control.interrupted() {
            return Err(MemoryAnalysisError::Interrupted);
        }
        centered_gram_apply(features, &mean, &basis[step], &mut projected, &mut w);
        alphas.push(dot(&w, &basis[step]));
        // Full reorthogonalization keeps the basis orthonormal in floating
        // point (it also removes the α·qₖ and β·qₖ₋₁ terms of the recurrence).
        for q in &basis {
            let coefficient = dot(&w, q);
            for (w_i, q_i) in w.iter_mut().zip(q) {
                *w_i -= coefficient * q_i;
            }
        }
        let beta = dot(&w, &w).sqrt();
        let k = alphas.len();
        // Rayleigh quotients never exceed the dominant eigenvalue, so the
        // largest one is a safe scale for the breakdown test.
        let scale = alphas.iter().copied().fold(0.0_f64, f64::max);
        let exhausted = beta <= RANK_FLOOR * scale;
        let last = step + 1 == max_steps;
        if exhausted || last || k.is_multiple_of(RITZ_CHECK_INTERVAL) {
            let pairs = ritz_pairs(&alphas, &betas)?;
            let dominant = pairs.values[0].max(0.0);
            let converged = k >= 2 && {
                let residual = |component: usize| beta * pairs.vectors[component][k - 1].abs();
                residual(0) <= CONVERGENCE_TOLERANCE * dominant
                    && residual(1) <= CONVERGENCE_TOLERANCE * dominant
            };
            ritz = Some(pairs);
            // `exhausted` means the Krylov space is invariant: the Ritz pairs
            // are exact and there is no further direction to add.
            if converged || exhausted || last {
                break;
            }
        }
        betas.push(beta);
        basis.push(w.iter().map(|x| x / beta).collect());
    }
    let ritz = match ritz {
        Some(pairs) => pairs,
        None => ritz_pairs(&alphas, &betas)?,
    };

    let mut scores = vec![[0.0_f64; 2]; n];
    let dominant = ritz.values.first().copied().unwrap_or(0.0);
    for (component, (&eigenvalue, coefficients)) in
        ritz.values.iter().zip(&ritz.vectors).take(2).enumerate()
    {
        if read_control.interrupted() {
            return Err(MemoryAnalysisError::Interrupted);
        }
        if eigenvalue <= RANK_FLOOR * dominant.max(RANK_FLOOR) {
            break;
        }
        let mut x = vec![0.0; n];
        for (coefficient, q) in coefficients.iter().zip(&basis) {
            for (x_i, q_i) in x.iter_mut().zip(q) {
                *x_i += coefficient * q_i;
            }
        }
        let sign = if dot(&x, &start) < 0.0 { -1.0 } else { 1.0 };
        let scale = eigenvalue.sqrt() * sign;
        for (score, value) in scores.iter_mut().zip(&x) {
            score[component] = value * scale;
        }
    }

    let max_abs = scores
        .iter()
        .flat_map(|s| s.iter())
        .fold(0.0_f64, |acc, v| acc.max(v.abs()));
    if max_abs > 0.0 {
        for s in &mut scores {
            s[0] /= max_abs;
            s[1] /= max_abs;
        }
    }
    Ok(Some(scores))
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// `out = Fc·(Fcᵀ·v)` for the centered features `Fc = F − 1·meanᵀ` without
/// materializing `Fc`: `Fcᵀ·v = Fᵀ·v − (Σv)·mean` and `Fc·w = F·w − (mean·w)·1`.
fn centered_gram_apply(
    features: &[Vec<f64>],
    mean: &[f64],
    v: &[f64],
    projected: &mut [f64],
    out: &mut [f64],
) {
    projected.fill(0.0);
    let mut v_sum = 0.0;
    for (row, weight) in features.iter().zip(v) {
        v_sum += weight;
        for (p, x) in projected.iter_mut().zip(row) {
            *p += weight * x;
        }
    }
    for (p, m) in projected.iter_mut().zip(mean) {
        *p -= v_sum * m;
    }
    let mean_dot = dot(mean, projected);
    for (o, row) in out.iter_mut().zip(features) {
        *o = dot(row, projected) - mean_dot;
    }
}

/// Eigenpairs of the Lanczos tridiagonal matrix, eigenvalues descending;
/// `vectors[i]` holds the `k` basis coefficients of the `i`-th Ritz vector.
struct RitzPairs {
    values: Vec<f64>,
    vectors: Vec<Vec<f64>>,
}

fn ritz_pairs(alphas: &[f64], betas: &[f64]) -> Result<RitzPairs, MemoryAnalysisError> {
    let k = alphas.len();
    let mut matrix = vec![vec![0.0; k]; k];
    for (i, alpha) in alphas.iter().enumerate() {
        matrix[i][i] = *alpha;
    }
    for (i, beta) in betas.iter().enumerate().take(k.saturating_sub(1)) {
        matrix[i][i + 1] = *beta;
        matrix[i + 1][i] = *beta;
    }
    symmetric_eigen(matrix)
}

/// Cyclic Jacobi eigen-decomposition of a small dense symmetric matrix.
fn symmetric_eigen(mut a: Vec<Vec<f64>>) -> Result<RitzPairs, MemoryAnalysisError> {
    let k = a.len();
    let mut v: Vec<Vec<f64>> = (0..k)
        .map(|i| (0..k).map(|j| if i == j { 1.0 } else { 0.0 }).collect())
        .collect();
    let mut converged = false;
    for _ in 0..JACOBI_MAX_SWEEPS {
        let mut off = 0.0;
        let mut diag = 0.0;
        for (i, row) in a.iter().enumerate() {
            for (j, value) in row.iter().enumerate() {
                if i == j {
                    diag += value * value;
                } else {
                    off += value * value;
                }
            }
        }
        // Rotations leave off-diagonals at rounding level (~ε·|a|); the
        // Frobenius bound `‖off‖ ≤ k·ε·‖diag‖` is reachable in floating point
        // and caps eigenvalue error at k·ε relative (Weyl).
        let bound = k as f64 * f64::EPSILON;
        if off <= bound * bound * diag {
            converged = true;
            break;
        }
        for p in 0..k {
            for q in (p + 1)..k {
                if a[p][q] == 0.0 {
                    continue;
                }
                let theta = (a[q][q] - a[p][p]) / (2.0 * a[p][q]);
                let t = if theta == 0.0 {
                    1.0
                } else {
                    theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt())
                };
                let c = 1.0 / (t * t + 1.0).sqrt();
                let s = t * c;
                for row in a.iter_mut() {
                    let (arp, arq) = (row[p], row[q]);
                    row[p] = c * arp - s * arq;
                    row[q] = s * arp + c * arq;
                }
                let (head, tail) = a.split_at_mut(q);
                for (apr, aqr) in head[p].iter_mut().zip(tail[0].iter_mut()) {
                    let (before_p, before_q) = (*apr, *aqr);
                    *apr = c * before_p - s * before_q;
                    *aqr = s * before_p + c * before_q;
                }
                for row in v.iter_mut() {
                    let (vrp, vrq) = (row[p], row[q]);
                    row[p] = c * vrp - s * vrq;
                    row[q] = s * vrp + c * vrq;
                }
            }
        }
    }
    if !converged {
        return Err(MemoryAnalysisError::ProjectionNotConverged { dimension: k });
    }
    let mut order: Vec<usize> = (0..k).collect();
    order.sort_by(|x, y| a[*y][*y].total_cmp(&a[*x][*x]));
    Ok(RitzPairs {
        values: order.iter().map(|i| a[*i][*i]).collect(),
        vectors: order
            .iter()
            .map(|i| v.iter().map(|row| row[*i]).collect())
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    /// Tolerance on normalized scores for agreement with the dense reference.
    /// Payload coordinates are rounded to 1e-6, so this is two orders tighter
    /// than anything a consumer can observe.
    const REFERENCE_TOLERANCE: f64 = 1e-8;

    fn read_control() -> FactReadControl {
        FactReadControl::new(Arc::new(|| false))
    }

    /// The dense implementation this module replaced: explicit centering,
    /// materialized Gram matrix, 200 power iterations per component with
    /// explicit deflation. Kept as the numerical reference.
    fn dense_reference_pca_scores(features: &[Vec<f64>]) -> Vec<[f64; 2]> {
        let n = features.len();
        let d = features[0].len();
        let mut mean = vec![0.0; d];
        for row in features {
            for (m, v) in mean.iter_mut().zip(row) {
                *m += v;
            }
        }
        for m in &mut mean {
            *m /= n as f64;
        }
        let centered: Vec<Vec<f64>> = features
            .iter()
            .map(|row| row.iter().zip(&mean).map(|(v, m)| v - m).collect())
            .collect();
        let mut gram = vec![vec![0.0; n]; n];
        for i in 0..n {
            for j in i..n {
                let value = dot(&centered[i], &centered[j]);
                gram[i][j] = value;
                gram[j][i] = value;
            }
        }
        let mut scores = vec![[0.0_f64; 2]; n];
        for component in 0..2 {
            let mut v: Vec<f64> = (0..n).map(|i| 1.0 + (i as f64 % 7.0) / 7.0).collect();
            let mut eigenvalue = 0.0;
            for _ in 0..200 {
                let next: Vec<f64> = gram.iter().map(|row| dot(row, &v)).collect();
                let norm = dot(&next, &next).sqrt();
                if norm < 1e-12 {
                    eigenvalue = 0.0;
                    break;
                }
                v = next.iter().map(|x| x / norm).collect();
                eigenvalue = norm;
            }
            if eigenvalue <= 1e-12 {
                break;
            }
            let scale = eigenvalue.sqrt();
            for (score, value) in scores.iter_mut().zip(&v) {
                score[component] = value * scale;
            }
            for i in 0..n {
                for j in 0..n {
                    gram[i][j] -= eigenvalue * v[i] * v[j];
                }
            }
        }
        let max_abs = scores
            .iter()
            .flat_map(|s| s.iter())
            .fold(0.0_f64, |acc, v| acc.max(v.abs()));
        for s in &mut scores {
            s[0] /= max_abs;
            s[1] /= max_abs;
        }
        scores
    }

    struct Lcg(u64);

    impl Lcg {
        fn unit(&mut self) -> f64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }

        fn signed(&mut self) -> f64 {
            self.unit() * 2.0 - 1.0
        }
    }

    /// Rows with two dominant, well-separated variance directions plus
    /// isotropic noise, so both power iteration and Lanczos converge fully
    /// and any disagreement is algorithmic rather than an unconverged tail.
    fn gapped_features(rows: usize, d: usize, seed: u64) -> Vec<Vec<f64>> {
        let mut rng = Lcg(seed);
        let axis_a: Vec<f64> = (0..d).map(|_| rng.signed()).collect();
        let axis_b: Vec<f64> = (0..d).map(|_| rng.signed()).collect();
        (0..rows)
            .map(|_| {
                let a = rng.signed() * 3.0;
                let b = rng.signed() * 1.2;
                (0..d)
                    .map(|j| 0.5 + a * axis_a[j] + b * axis_b[j] + rng.signed() * 0.05)
                    .collect()
            })
            .collect()
    }

    fn max_abs_difference(left: &[[f64; 2]], right: &[[f64; 2]]) -> f64 {
        left.iter()
            .zip(right)
            .flat_map(|(l, r)| [(l[0] - r[0]).abs(), (l[1] - r[1]).abs()])
            .fold(0.0, f64::max)
    }

    #[test]
    fn pca_scores_observes_live_interruption() {
        let control = FactReadControl::new(Arc::new(|| true));
        let error = pca_scores(&[vec![1.0], vec![2.0]], &control)
            .expect_err("interrupted PCA must fail before projection");
        assert_eq!(error, MemoryAnalysisError::Interrupted);
    }

    #[test]
    fn pca_scores_match_the_dense_gram_reference_within_tolerance() {
        for (rows, d, seed) in [(60_usize, 16_usize, 3_u64), (150, 96, 11), (40, 300, 29)] {
            let features = gapped_features(rows, d, seed);
            let reference = dense_reference_pca_scores(&features);
            let scores = pca_scores(&features, &read_control())
                .expect("PCA must not be interrupted")
                .expect("gapped features must project");
            let difference = max_abs_difference(&reference, &scores);
            assert!(
                difference <= REFERENCE_TOLERANCE,
                "rows={rows} d={d}: max |reference - lanczos| = {difference:e} exceeds {REFERENCE_TOLERANCE:e}"
            );
            let max_abs = scores
                .iter()
                .flat_map(|s| s.iter())
                .fold(0.0_f64, |acc, v| acc.max(v.abs()));
            assert!(
                (max_abs - 1.0).abs() < 1e-12,
                "scores must be max-normalized"
            );
        }
    }

    #[test]
    fn rank_one_features_leave_the_second_component_zero() {
        let features: Vec<Vec<f64>> = (0..6)
            .map(|t| vec![t as f64, 2.0 * t as f64, -3.0 * t as f64])
            .collect();
        let scores = pca_scores(&features, &read_control())
            .expect("PCA must not be interrupted")
            .expect("collinear features still project");
        assert!(scores.iter().all(|s| s[1] == 0.0));
        assert!(scores.iter().any(|s| s[0].abs() == 1.0));
        // Collinear points keep their order along the single component.
        assert!(scores.windows(2).all(|pair| pair[0][0] < pair[1][0]));
    }

    #[test]
    fn non_finite_features_are_a_typed_error() {
        let features = vec![vec![1.0, 0.0], vec![f64::NAN, 1.0], vec![0.0, 2.0]];
        let error = pca_scores(&features, &read_control())
            .expect_err("NaN input must not project silently");
        assert_eq!(error, MemoryAnalysisError::NonFiniteFeature { index: 1 });
    }
}
