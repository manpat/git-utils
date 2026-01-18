use anyhow::Context;
use clap::Parser;

use std::io::{stdout};
use std::process::ExitCode;
use std::path::PathBuf;

use crossterm::{
	*,
	tty::*,
	event::*,
};

mod git;
mod ui;

use git::*;

#[derive(Parser, Debug)]
#[command(version, author, about)]
struct MainArgs {
	/// Log git commands to file.
	#[arg(long, global=true)]
	log: bool,

	/// Override working directory.
	#[arg(long, global=true)]
	working_dir: Option<PathBuf>,

	#[command(subcommand)]
	subcommand: ArgCommand,
}

#[derive(clap::Subcommand, Debug)]
enum ArgCommand {
	/// Install git aliases
	Install {
		/// Install aliases for this user.
		/// This is the default.
		#[arg(long, short, group = "scope")]
		user: bool,

		/// Install aliases for the whole system - will be available to all users.
		#[arg(long, short, group = "scope")]
		system: bool,

		/// Install aliases only in this repo.
		#[arg(long, short, group = "scope")]
		local: bool,
	},

	/// Interactively switch branches
	Switch {
		/// Create/switch to a remote tracking branch
		#[arg(long, short)]
		remote: bool,
	},

	// SearchCommits
	// DeleteBranches

	// /// 
	// CreateBranch {
	// 	/// Create branch from a commit instead of a branch
	// 	#[arg(long, short)]
	// 	commit: bool,
	// },
}


fn main() -> ExitCode {
	match run() {
		Err(err) => {
			eprintln!("{err}");
			ExitCode::FAILURE
		}

		Ok(()) => ExitCode::SUCCESS
	}
}


fn run() -> anyhow::Result<()> {
	let args = MainArgs::parse();

	if !stdout().is_tty() {
		anyhow::bail!("Can only be run in an interactive tty");
	}

	if args.log {
		use std::fs::File;
		let log_file = File::create("git-utils.log")?;
		simplelog::WriteLogger::init(log::LevelFilter::Info, simplelog::Config::default(), log_file)?;
	}

	let _guard = on_drop(|| {
		execute!{
			stdout(),
			style::ResetColor,
		}.unwrap();
	});

	let git = GitContext::new(&args);

	match args.subcommand {
		ArgCommand::Install { system, local, .. } => {
			let current_path = std::env::current_exe()?;

			let current_path_fixed = if cfg!(windows) {
				use std::ffi::{OsString};
				use std::path::{PathBuf, Component};

				let mut current_path_string = OsString::with_capacity(current_path.as_os_str().len());
				for component in current_path.components() {
					match component {
						Component::Prefix(prefix) => current_path_string.push(prefix.as_os_str()),
						Component::RootDir => {},
						other => {
							// Git seems to require unix paths
							current_path_string.push("/");
							current_path_string.push(other);
						}
					}
				}

				PathBuf::from(current_path_string)

			} else {
				current_path.to_path_buf()
			};


			let mut args = ["config", "set"].to_vec();

			if system {
				args.push("--system");
			} else if local {
				args.push("--local");
			} else {
				args.push("--global");
			}

			let aliases = [
				("iswitch", "switch"),
			];

			for (alias, command) in aliases {
				let config_name = format!("alias.{alias}");
				let config_command = format!("!{} {command}", current_path_fixed.display());
				git.run(args.iter().cloned().chain([config_name.as_str(), config_command.as_str()]))?;

				println!("Aliasing `git {alias}` to `git-utils {command}`");
			}
		}

		ArgCommand::Switch { remote } => {
			if !detect_clean_worktree_and_index(&git)? {
				anyhow::bail!("There are changes in the index/worktree which must be committed, reverted, or stashed before switching branches");
			}

			let refspec = match remote {
				false => "refs/heads",
				true => "refs/remotes"
			};

			// TODO(pat.m): include upstream in list
			let mut full_branch_list = git.query_list(["for-each-ref", "--format", "%(refname:lstrip=2)", refspec])?;
			full_branch_list.retain(|branch| !branch.ends_with("/HEAD"));
			if full_branch_list.is_empty() {
				anyhow::bail!("No branches to switch to.");
			}

			let recent_branches = get_recent_branch_list(&git, remote)?;

			let mut list_model = ui::FilterableList::new();

			// Push recent branches _that still exist_ first, in the order they appear in reflog.
			for recent_branch in recent_branches.iter() {
				if let Some(position) = full_branch_list.iter().position(|branch| recent_branch == branch) {
					let branch = full_branch_list.remove(position);
					list_model.insert_formatted(branch);
				}
			}

			// Push remaining non-recent branches in original order
			for recent_branch in full_branch_list {
				list_model.insert_formatted(recent_branch);
			}

			// let index = list_prompt(&ordered_branch_list)?;
			// let selected_branch = ordered_branch_list[index].as_str();
			let selected_branch = list_model.run()?;

			if remote {
				let (_remote, local_branch) = selected_branch.split_once('/').context("git for-each-ref yielded info in unexpected format")?;

				if ref_exists(&git, &format!("refs/heads/{local_branch}"))? {
					match get_upstream(&git, local_branch)? {
						Some(current_upstream) => {
							if current_upstream != selected_branch {
								anyhow::bail!("Branch with name '{local_branch}' already exists but has different tracking branch '{current_upstream}' (expected '{selected_branch}')")
							}
						}

						None => {
							anyhow::bail!("Branch with name '{local_branch}' already exists but isn't tracking requested branch '{selected_branch}'");
						}
					}

					git.run(["switch", local_branch])?;
					println!("Switched to branch {local_branch}, tracking {selected_branch}");
				} else {
					git.run(["switch", "--track", &selected_branch, "--create", local_branch])?;
					println!("Switched to new branch {local_branch}, tracking {selected_branch}");
				}

			} else {
				git.run(["switch", &selected_branch])?;
				println!("Switched to branch {selected_branch}");
			}
		}
	}

	Ok(())
}

fn detect_clean_worktree_and_index(git: &GitContext) -> anyhow::Result<bool> {
	let modifications = git.query_list(["status", "--porcelain=1", "--untracked-files=no", "--ignored=no"])?;
	Ok(modifications.is_empty())
}

fn get_recent_branch_list(git: &GitContext, remote: bool) -> anyhow::Result<Vec<String>> {
	let reflog = git.query_list(["log", "--walk-reflogs", "--decorate=full", "-n100", "--format=format:%(decorate:prefix=,suffix=,pointer=>>>,separator=%x2c)"])?;

	let ref_prefix = match remote {
		false => "refs/heads/",
		true => "refs/remotes/",
	};

	let mut branches: Vec<String> = Vec::with_capacity(reflog.len());
	for entry in reflog {
		if entry.trim().is_empty() || entry.contains(">>>") {
			continue
		}

		for reference in entry.split(',') {
			let Some(refname) = reference.strip_prefix(ref_prefix) else {
				continue
			};

			if branches.iter().any(|branch| branch == refname) {
				continue
			}

			branches.push(refname.to_owned());
		}
	}

	Ok(branches)
}

fn ref_exists(git: &GitContext, refname: &str) -> anyhow::Result<bool> {
	git.query_success(["show-ref", "--quiet", refname])
}

fn get_upstream(git: &GitContext, branch: &str) -> anyhow::Result<Option<String>> {
	git.try_query(["rev-parse", "--quiet", "--abbrev-ref", "--verify", &format!("{branch}@{{upstream}}")])
}



pub fn on_drop(f: impl FnOnce()) -> impl Drop {
	use std::mem::ManuallyDrop;

	#[must_use]
	struct DropGuard<F: FnOnce()>(ManuallyDrop<F>);
	impl<F: FnOnce()> Drop for DropGuard<F> {
		fn drop(&mut self) {
			let f = unsafe{ ManuallyDrop::take(&mut self.0) };
			f();
		}
	}

	DropGuard(ManuallyDrop::new(f))
}
