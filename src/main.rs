use anyhow::Context;
use clap::Parser;
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;

use std::io::{stdout};
use std::process::ExitCode;
use std::path::PathBuf;

use crossterm::{
	*,
	tty::*,
	event::*,
};

mod git;

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

			let mut ordered_branch_list = Vec::with_capacity(full_branch_list.len());

			// Push recent branches _that still exist_ first, in the order they appear in reflog.
			for recent_branch in recent_branches.iter() {
				if let Some(position) = full_branch_list.iter().position(|branch| recent_branch == branch) {
					let branch = full_branch_list.remove(position);
					ordered_branch_list.push(branch);
				}
			}

			// Push remaining non-recent branches in original order
			ordered_branch_list.extend(full_branch_list);

			let index = list_prompt(&ordered_branch_list)?;
			let selected_branch = ordered_branch_list[index].as_str();

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
					git.run(["switch", "--track", selected_branch, "--create", local_branch])?;
					println!("Switched to new branch {local_branch}, tracking {selected_branch}");
				}

			} else {
				git.run(["switch", selected_branch])?;
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



fn list_prompt<I: std::fmt::Display>(items: &[I]) -> anyhow::Result<usize> {
	anyhow::ensure!(!items.is_empty());

	let mut out = stdout();

	let mut selected_index = 0usize;
	let mut caret_index = 0usize;
	let mut offset = 0;
	let mut filter_string = String::new();

	let (_, mut terminal_height) = terminal::size()?;
	let desired_height = terminal_height.min(items.len() as u16 + 1);
	let mut max_visible_items = desired_height as usize - 1;

	// Clear enough space
	{
		let num_newlines = desired_height.saturating_sub(1);
		for _ in 0..num_newlines { print!("\n"); }
		execute!{out, cursor::MoveUp(num_newlines)}?;
	}

	execute!{
		out,
		terminal::DisableLineWrap,
	}?;

	let start_row = cursor::position()?.1;

	let _guard = on_drop(|| {
		execute!{
			stdout(),
			cursor::MoveTo(0, start_row),
			terminal::Clear(terminal::ClearType::FromCursorDown),
			style::ResetColor,
			terminal::EnableLineWrap,
		}.unwrap();
	});

	let matcher = SkimMatcherV2::default();
	let item_strings: Vec<_> = items.iter().map(|item| item.to_string()).collect();

	#[derive(Ord, PartialOrd, Eq, PartialEq)]
	struct FilteredItem<'s> {
		score: i64,
		original_index: usize,
		text: &'s str,
	}

	let mut filtered_items: Vec<_> = item_strings.iter().enumerate()
		.map(|(index, item)| FilteredItem {
			score: 0,
			original_index: index,
			text: item,
		})
		.collect();

	'main: loop {
		execute!{
			out,
			terminal::BeginSynchronizedUpdate,

			cursor::MoveTo(0, start_row),
			terminal::Clear(terminal::ClearType::FromCursorDown),
			style::Print("Switch to branch: "),
		}?;

		let cursor_start = cursor::position()?.0;

		print!("{filter_string}");

		// Render list.
		for (index, &FilteredItem{ text, .. }) in filtered_items.iter().enumerate().skip(offset).take(max_visible_items) {
			let is_selected = index == selected_index;
			let marker = match is_selected {
				true => '>',
				false => ' ',
			};

			if is_selected {
				queue!{
					out, 
					style::SetForegroundColor(style::Color::Black),
					style::SetBackgroundColor(style::Color::White),
				}?;
			}

			print!("\n{marker} {text}{}", style::ResetColor);
		}

		execute!{
			out,
			cursor::MoveTo(cursor_start + caret_index as u16, start_row),
			terminal::EndSynchronizedUpdate,
		}?;

		let _guard = start_raw_mode()?;

		'events: loop {
			match event::read()? {
				Event::Key(KeyEvent{ code, modifiers, kind: KeyEventKind::Press, .. }) => {
					match (code, modifiers) {
						(KeyCode::Enter, _) if !filtered_items.is_empty() => break 'main,

						(KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Esc, _) => {
							anyhow::bail!("Cancelled")
						}

						// Note: ctrl+backspace produces ^h on my machine.
						(KeyCode::Backspace, KeyModifiers::CONTROL) | (KeyCode::Char('h'), KeyModifiers::CONTROL) => {
							// Not quite right but whatever
							filter_string.clear();
							caret_index = 0;
						}

						(KeyCode::Backspace, _) => if let Some(index) = caret_index.checked_sub(1) {
							filter_string.remove(index);
							caret_index -= 1;
						}

						(KeyCode::Delete, _) => if !filter_string.is_empty() {
							filter_string.remove(caret_index);
						}

						(KeyCode::Home, _) => { caret_index = 0; }
						(KeyCode::End, _) => { caret_index = filter_string.len(); }

						(KeyCode::Left, _) => { caret_index = caret_index.saturating_sub(1); }
						(KeyCode::Right, _) => { caret_index += 1; }

						(KeyCode::Up, _) => { selected_index = selected_index.saturating_sub(1); }
						(KeyCode::Down, _) => { selected_index += 1; }

						(KeyCode::PageUp, KeyModifiers::CONTROL) => { selected_index = 0; }
						(KeyCode::PageDown, KeyModifiers::CONTROL) => { selected_index = filtered_items.len(); }

						(KeyCode::PageUp, _) => { selected_index = selected_index.saturating_sub(terminal_height as usize - 1); }
						(KeyCode::PageDown, _) => { selected_index += terminal_height as usize - 1; }

						(KeyCode::Char(ch), _) => if ch.is_ascii() {
							filter_string.insert(caret_index, ch);
							caret_index += 1;
						}

						_ => {}
					}

					break 'events
				}

				Event::Resize(width, height) => {
					terminal_height = height;
					let desired_height = terminal_height.min(items.len() as u16 + 1);
					max_visible_items = desired_height as usize - 1;
					break 'events
				}

				_ => {}
			}
		}

		// Refilter
		filtered_items.clear();
		filtered_items.extend(
			item_strings.iter().enumerate()
				.filter_map(|(index, item)| {
					matcher.fuzzy_match(item, &filter_string)
						.map(|score| FilteredItem {
							score: -score,
							original_index: index,
							text: item.as_str(),
						})
				})
		);

		filtered_items.sort();

		// Keep indices in bounds
		caret_index = caret_index.min(filter_string.len());

		if !filtered_items.is_empty() {
			selected_index = selected_index.min(filtered_items.len() - 1);
		}

		// Make sure selection is in view
		if selected_index >= offset + max_visible_items {
			offset = selected_index - max_visible_items + 1;
		} else if selected_index < offset {
			offset = selected_index;
		}
	}

	anyhow::ensure!(selected_index < filtered_items.len());

	Ok(filtered_items[selected_index].original_index)
}


fn start_raw_mode() -> anyhow::Result<impl Drop> {
	terminal::enable_raw_mode()?;
	Ok(on_drop(|| {
		terminal::disable_raw_mode().unwrap()
	}))
}


fn on_drop(f: impl FnOnce()) -> impl Drop {
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
