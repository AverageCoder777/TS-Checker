#[test]
fn repro_route_check_hang() {
    let route = std::path::PathBuf::from(r"X:\SteamLibrary\steamapps\common\RailWorks\Content\Routes\8378d7b6-30ad-4c29-a5ac-16d3c2b26ff7");
    let rw = std::path::PathBuf::from(r"X:\SteamLibrary\steamapps\common\RailWorks");
    let assets = rw.join("Assets");
    let serz = rw.join("serz64.exe"); // guess
    println!("serz exists: {}", serz.is_file());
    // find serz
    let serz = if serz.is_file() { serz } else {
        // search
        let mut found = None;
        for c in ["serz.exe","serz64.exe"] {
            let p = rw.join(c);
            if p.is_file() { found = Some(p); break; }
        }
        found.unwrap_or_else(|| rw.join("serz.exe"))
    };
    println!("using serz: {}", serz.display());
    let start = std::time::Instant::now();
    let res = check_route(&route, &assets, &serz, true, 8, |msg| {
        println!("[{}] {}", start.elapsed().as_secs_f32(), msg);
    });
    println!("RESULT after {:?}: {:?}", start.elapsed(), res.map(|r| (r.route_files, r.converted_bin_files, r.referenced_assets, r.missing_assets.len(), r.conversion_errors.len(), r.structure_issues.len())));
}
