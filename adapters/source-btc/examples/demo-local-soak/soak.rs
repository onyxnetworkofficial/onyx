//! `--soak-rounds N` (scripts/soak.sh, ROADMAP L6): after the rehearsal, keep
//! settling new rounds on the same deployment, one every `--soak-every`
//! seconds, so a running `onyx-node` sees new `Finalized` events while it is
//! under load. Each round is an honest round (2 miners, the assigned
//! validators, `finalizeShard`, the challenge window, `release`) on a
//! sub-range of the live blocks (a new shard id each), registered in
//! `OnyxRevenue` (fork mode) so its rows can be sold. The shards are mined and
//! exported (held, rows in the row store) before the node starts, as an
//! operator who holds a shard before it is final.

use super::*;

/// Round numbers of the soak rounds start here (1-3 are the rehearsal's).
pub const FIRST_ROUND: u64 = 100;

/// One planned soak round: its data, the miners' works and their rows.
pub struct Shard {
    round: u64,
    d: Data,
    works: Vec<Work>,
    recipe_secs: f64,
}

/// `n` distinct block ranges inside `from..=to`, each ending before `to`
/// (round 1's own range ends there): 2-block ranges first, then 3-block
/// ones, and so on.
pub fn ranges(from: u64, to: u64, n: u64) -> Res<Vec<(u64, u64)>> {
    let mut out = Vec::new();
    let mut len = 2;
    while (out.len() as u64) < n {
        if from + len > to {
            return Err(format!(
                "--soak-rounds {n}: blocks {from}-{to} hold only {} sub-ranges",
                out.len()
            )
            .into());
        }
        let mut a = from;
        while a + len - 1 < to && (out.len() as u64) < n {
            out.push((a, a + len - 1));
            a += 1;
        }
        len += 1;
    }
    Ok(out)
}

/// Mine every planned round's shard and add it to the exported catalog
/// (held) and its rows to the exported row store.
pub fn prepare(net: &Net, o: &Opts, n: u64) -> Res<Vec<Shard>> {
    use onyx_node::catalog_file::{CatalogFile, HeldEntry};
    let live = o
        .live_dir
        .as_deref()
        .ok_or("--soak-rounds needs the live block files (--live-dir)")?;
    let dir = o
        .export_node
        .as_deref()
        .ok_or("--soak-rounds needs --export-node")?;
    let path = dir.join("catalog.json");
    let mut file = CatalogFile::from_json(&std::fs::read_to_string(&path)?)?;
    let store = onyx_coordinator::cas::FsCas::new(dir.join("cas"));
    let mut out = Vec::new();
    for (k, (a, b)) in ranges(o.live_from, o.live_to, n)?.into_iter().enumerate() {
        let d = data(Some((live, a, b)), false)?;
        if !d.label.starts_with("live") {
            return Err(format!("--soak-rounds: no live block files for {a}-{b}").into());
        }
        let round = FIRST_ROUND + k as u64;
        let (works, recipe_secs) = mine(&net.miners, round, &d)?;
        let w = works.first().ok_or("no miner")?;
        if works.iter().any(|x| x.commitment.root != w.commitment.root) {
            return Err(format!("soak round {round}: the miners disagree").into());
        }
        net.miners[0].publish_rows(w, &*d.cas)?;
        let rows = d
            .cas
            .get(Kind::Rows, &w.commitment.root)
            .ok_or("soak round: rows missing after publish")?;
        store.put(Kind::Rows, &rows)?;
        let rights = d
            .reg
            .source_rights(&SourceId(SOURCE_ID.into()))
            .ok_or("source not registered")?
            .clone();
        file.held.push(HeldEntry::new(
            METRIC,
            &d.spec,
            &rights,
            Some(w.commitment.leaf_count),
            Some(&d.end_header),
        )?);
        out.push(Shard {
            round,
            d,
            works,
            recipe_secs,
        });
    }
    file.to_catalog()?;
    std::fs::write(&path, serde_json::to_string_pretty(&file)? + "\n")?;
    for p in file.params_blobs()? {
        store.put(Kind::Blob, &p)?;
    }
    println!(
        "  soak: {} more rounds planned (rounds {}-{}), held in {}",
        out.len(),
        FIRST_ROUND,
        FIRST_ROUND + out.len() as u64 - 1,
        path.display()
    );
    Ok(out)
}

/// Settle one planned round to the end: open, commit, reveal, validate,
/// finalize, the challenge window, release and (fork) the `OnyxRevenue`
/// registration. Returns a line for the log.
pub fn play(net: &mut Net, paid: Option<&Paid>, s: &Shard) -> Res<String> {
    let t = Instant::now();
    let (coord, usdc, st) = (net.coord, net.dep.usdc, net.st());
    // The round's reward comes from the pool: top it up by one reward.
    net.chain.send(
        &coord,
        &usdc,
        "mint(address,uint256)",
        &[ax(&coord), USDC.to_string()],
    )?;
    net.chain
        .send(&coord, &st, "fundRewards(uint256)", &[USDC.to_string()])?;
    let p = net.play(s.round, &s.d, &s.works, s.recipe_secs)?;
    let status = net.status(s.round, &p.shard)?;
    if status != FINALIZED {
        return Ok(format!(
            "soak round {} ({}): status {status}, not final (too few standbys); skipped",
            s.round, s.d.label
        ));
    }
    let fin = net.chain.block()?;
    net.chain.reach(fin + p.cfg.dispute.challenge_window)?;
    let dsp = net.dep.disputes;
    net.chain.send(
        &coord,
        &dsp,
        "release(uint64,bytes32)",
        &[s.round.to_string(), hx(&p.shard)],
    )?;
    if let Some(pd) = paid {
        let f = s.works[0].commitment.fields();
        net.chain.send(
            &coord,
            &pd.revenue,
            "register(uint64,bytes32,(bytes32,uint64,bytes32,bytes32,bytes32,bytes32,bytes32,bytes32))",
            &[
                s.round.to_string(),
                hx(&p.shard),
                format!(
                    "({},{},{},{},{},{},{},{})",
                    hx(&f.root),
                    f.leaf_count,
                    hx(&f.recipe_id),
                    hx(&f.runtime_hash),
                    hx(&f.label_set_hash),
                    hx(&f.source_hash),
                    hx(&f.schema_id),
                    hx(&f.rights_hash)
                ),
            ],
        )?;
    }
    Ok(format!(
        "soak round {} ({}): shard {} root {} finalized at block {fin}, released{} in {:.0} s",
        s.round,
        s.d.label,
        hx(&p.shard),
        hx(&s.works[0].commitment.root),
        if paid.is_some() {
            " and registered in OnyxRevenue"
        } else {
            ""
        },
        t.elapsed().as_secs_f64()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_are_distinct_and_end_before_round_one() {
        let r = ranges(970_543, 970_566, 40).expect("40 ranges");
        assert_eq!(r.len(), 40);
        assert_eq!(r[0], (970_543, 970_544));
        assert_eq!(r[22], (970_543, 970_545));
        let set: BTreeSet<_> = r.iter().collect();
        assert_eq!(set.len(), 40);
        assert!(r
            .iter()
            .all(|(a, b)| a < b && *b < 970_566 && *a >= 970_543));
        assert!(ranges(970_543, 970_545, 5).is_err());
    }
}
