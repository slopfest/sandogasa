mod compare_provides;
mod fedrq;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "Hyperscale package intake tool")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Compare the Provides of a source package between two branches.
    CompareProvides {
        /// Source RPM name (e.g. "systemd").
        srpm: String,
        /// Branch to compare from (e.g. "rawhide").
        source_branch: String,
        /// Branch to compare to (e.g. "c10s-hyperscale").
        target_branch: String,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::CompareProvides {
            srpm,
            source_branch,
            target_branch,
        } => {
            let result =
                compare_provides::compare_provides(&srpm, &source_branch, &target_branch);

            match result {
                Ok(cmp) => {
                    if cmp.added.is_empty() && cmp.removed.is_empty() && cmp.upgraded.is_empty() {
                        println!("No differences in Provides.");
                        return;
                    }
                    let mut need_blank = false;
                    if !cmp.upgraded.is_empty() {
                        let name_w = cmp.upgraded.iter().map(|u| u.name.len()).max().unwrap();
                        let src_w = cmp.upgraded.iter().map(|u| u.source_version.len()).max().unwrap()
                            .max(source_branch.len());
                        let tgt_w = cmp.upgraded.iter().map(|u| u.target_version.len()).max().unwrap()
                            .max(target_branch.len());

                        let sep = format!("+-{}-+-{}-+-{}-+", "-".repeat(name_w), "-".repeat(src_w), "-".repeat(tgt_w));
                        println!("Upgraded ({source_branch} -> {target_branch}):");
                        println!("{sep}");
                        println!("| {:<name_w$} | {:<src_w$} | {:<tgt_w$} |", "Provide", source_branch, target_branch);
                        println!("{sep}");
                        for u in &cmp.upgraded {
                            println!("| {:<name_w$} | {:<src_w$} | {:<tgt_w$} |", u.name, u.source_version, u.target_version);
                        }
                        println!("{sep}");
                        need_blank = true;
                    }
                    if !cmp.removed.is_empty() {
                        if need_blank { println!(); }
                        println!("Removed (in {source_branch} but not {target_branch}):");
                        for p in &cmp.removed {
                            println!("  - {p}");
                        }
                        need_blank = true;
                    }
                    if !cmp.added.is_empty() {
                        if need_blank { println!(); }
                        println!("Added (in {target_branch} but not {source_branch}):");
                        for p in &cmp.added {
                            println!("  + {p}");
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    std::process::exit(1);
                }
            }
        }
    }
}
