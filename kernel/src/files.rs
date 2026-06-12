//! Kernel-zijde van **EuroFiles** (Sprint AC-1): de bestandsbeheerder.
//! Bij boot bewijzen we map-sortering (mappen eerst), filteren, padnormalisatie
//! en de soevereine badges. Host-geteste kern: [`eurofiles`].

use crate::serial_println;
use eurofiles::{join, normalize, Badge, DirEntry, Listing, SortKey, SortOrder};

/// Boot-zelftest: bouw een maplijst, sorteer/filter, controleer padbewerkingen.
pub fn selftest() {
    let mut l = Listing::new(
        "/etc//euro/../euro",
        alloc::vec![
            DirEntry::file("zoem.txt", 1500),
            DirEntry::dir("conf"),
            DirEntry::file("kernel.efi", 2_500_000)
                .with_badge(Badge::Immutable)
                .with_badge(Badge::Signed),
            DirEntry::file(".verborgen", 10),
            DirEntry::dir("Assets"),
        ],
    );
    l.sort(SortKey::Name, SortOrder::Asc);
    let first_two: alloc::vec::Vec<&str> = l.entries.iter().take(2).map(|e| e.name.as_str()).collect();
    let dirs_first = first_two == alloc::vec!["Assets", "conf"];

    let visible = l.filter("", false).len(); // verborgen weg → 4
    let hits = l.filter("kernel", true).len(); // 1
    let (dirs, files) = l.counts();

    let path_ok = l.path == "/etc/euro"
        && normalize("/a/b/../c") == "/a/c"
        && join("/home/user", "docs/../x.md") == "/home/user/x.md";

    let kernel_signed = l
        .entries
        .iter()
        .find(|e| e.name == "kernel.efi")
        .map(|e| e.badges.contains(&Badge::Immutable) && e.badges.contains(&Badge::Signed))
        .unwrap_or(false);

    let ok = dirs_first && visible == 4 && hits == 1 && dirs == 2 && files == 3 && path_ok && kernel_signed;
    serial_println!(
        "[fl] EuroFiles: pad={}, {} mappen/{} bestanden, mappen-eerst={}, zichtbaar={} (van 5), kernel.efi🔒getekend={} {}",
        l.path,
        dirs,
        files,
        dirs_first,
        visible,
        kernel_signed,
        if ok { "✓" } else { "✗ FOUT" }
    );
}
