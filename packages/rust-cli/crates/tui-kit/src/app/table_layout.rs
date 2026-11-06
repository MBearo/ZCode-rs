//! 表格的列宽分配：按宽度铺表的纯整数分配核心，与 [`Layout`](super::layout::Layout) 分开，不用终端就能测。
//! 移植自 packages/tui/tui/src/zcode/tui/app/TableLayout.scala（feat/tui a2bf878b34）。
//!
//! 规则是 CSS 2.1 §17.5.2 的「自动表格布局」：
//! - 各列的自然宽度放得下时，各取自然宽度；
//! - 否则分享宽度，每列从自然宽度向最小宽度收，收多少与它的余量成比例；
//! - 连最小宽度都放不下时，按比例缩最小宽度（格子随后按字符断开）。
//!
//! 取整用最大余数法，结果之和正好是 `avail`（`avail` 够每列一格时）。算式与 Scala 相同，用 `f64`（JS 的 double）。

/// 和为 `avail`（可行时）的列宽。`natural[c]` 是列不折行的内容宽度，`min[c]` 是它合理的最小宽度；都 ≥ 1。
pub fn column_widths(natural: &[usize], min: &[usize], avail: usize) -> Vec<usize> {
    if natural.is_empty() {
        return Vec::new();
    }
    let sum_nat: usize = natural.iter().sum();
    let sum_min: usize = min.iter().sum();
    if sum_nat <= avail {
        natural.to_vec()
    } else if sum_min <= avail {
        // 多出的宽度按各列的余量（natural − min）分给最小宽度。
        let surplus = avail - sum_min;
        let flex: Vec<f64> = natural
            .iter()
            .zip(min)
            .map(|(&nat, &mn)| nat as f64 - mn as f64)
            .collect();
        let shares = largest_remainder(&flex, surplus);
        min.iter().zip(shares).map(|(&mn, s)| mn + s).collect()
    } else {
        // 连最小宽度都放不下：按可用宽度缩放它们。
        let weights: Vec<f64> = min.iter().map(|&m| m as f64).collect();
        largest_remainder(&weights, avail)
            .into_iter()
            .map(|w| w.max(1))
            .collect()
    }
}

/// 把 `total` 个整数单位按 `weights` 的比例分进各桶，剩下的单位给小数部分最大的桶。
fn largest_remainder(weights: &[f64], total: usize) -> Vec<usize> {
    let n = weights.len();
    if n == 0 || total == 0 {
        return vec![0; n];
    }
    let sum_w: f64 = weights.iter().sum();
    let effective: Vec<f64> = if sum_w <= 0.0 {
        vec![1.0; n]
    } else {
        weights.to_vec()
    };
    let eff_sum: f64 = effective.iter().sum();
    let exact: Vec<f64> = effective
        .iter()
        .map(|&w| total as f64 * w / eff_sum)
        .collect();
    // 权重非负时 exact 非负，floor 落在 usize 里；负权重（Scala 允许）在这里按 0 截断。
    let floors: Vec<usize> = exact.iter().map(|e| e.floor().max(0.0) as usize).collect();
    let mut remaining = total.saturating_sub(floors.iter().sum());
    // 按小数部分从大到小排（相同时按下标，稳定）。
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        let fa = -(exact[a] - exact[a].floor());
        let fb = -(exact[b] - exact[b].floor());
        fa.total_cmp(&fb).then(a.cmp(&b))
    });
    let mut result = floors;
    for &i in &order {
        if remaining == 0 {
            break;
        }
        result[i] += 1;
        remaining -= 1;
    }
    result
}
