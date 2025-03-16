use std::process::{Command, ExitCode};
use clap::Parser;
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;

use std::io::{stdout};
use crossterm::{
	*,
	tty::*,
	event::*,
};

#[derive(Parser, Debug)]
#[command(version, author, about)]
struct MainArgs {
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
	Switch,
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

	execute!{
		stdout(),
		event::PushKeyboardEnhancementFlags(event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
	}?;

	let _guard = on_drop(|| {
		execute!{
			stdout(),
			event::PopKeyboardEnhancementFlags,
			style::ResetColor,
		}.unwrap();
	});

	match args.subcommand {
		ArgCommand::Install { system, local, .. } => {
			let current_path = std::env::current_exe()?;

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
				let config_command = format!("!{} {command}", current_path.display());
				git(args.iter().cloned().chain([config_name.as_str(), config_command.as_str()]))?;

				println!("Aliasing `git {alias}` to `git-utils {command}`");
			}
		}

		ArgCommand::Switch => {
			let branch_list = git_list(["for-each-ref", "--format", "%(refname:short)", "refs/heads"])?;
			let index = list_prompt(&branch_list)?;
			git(["switch", &branch_list[index]])?;

			println!("Switched to branch {}", branch_list[index]);
		}
	}

	Ok(())
}

fn list_prompt<I: std::fmt::Display>(items: &[I]) -> anyhow::Result<usize> {
	anyhow::ensure!(!items.is_empty());

	let mut out = stdout();

	let mut selected_index = 0usize;
	let mut cursor_index = 0usize;
	let mut offset = 0;
	let mut filter_string = String::new();

	let (_, height) = terminal::size()?;
	let desired_height = height.min(items.len() as u16 + 1);
	let max_visible_items = desired_height as usize - 1;

	// Clear enough space
	{
		let num_newlines = desired_height.saturating_sub(1);
		for _ in 0..num_newlines { print!("\n"); }
		execute!{out, cursor::MoveUp(num_newlines)}?;
	}

	let start_row = cursor::position()?.1;

	let _guard = on_drop(|| {
		execute!{
			stdout(),
			cursor::MoveTo(0, start_row),
			terminal::Clear(terminal::ClearType::FromCursorDown),
			style::ResetColor,
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

	loop {
		execute!{
			out,
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
			cursor::MoveTo(cursor_start + cursor_index as u16, start_row),
		}?;

		let _guard = start_raw_mode()?;

		match event::read()? {
			Event::Key(KeyEvent{ code, modifiers, kind: KeyEventKind::Press, .. }) => match (code, modifiers) {
				(KeyCode::Enter, _) if !filtered_items.is_empty() => break,

				(KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Esc, _) => {
					anyhow::bail!("Cancelled")
				}

				// Note: ctrl+backspace produces ^h on my machine.
				(KeyCode::Backspace, KeyModifiers::CONTROL) | (KeyCode::Char('h'), KeyModifiers::CONTROL) => {
					// Not quite right but whatever
					filter_string.clear();
					cursor_index = 0;
				}

				(KeyCode::Backspace, _) => if let Some(index) = cursor_index.checked_sub(1) {
					filter_string.remove(index);
					cursor_index -= 1;
				}

				(KeyCode::Delete, _) => if !filter_string.is_empty() {
					filter_string.remove(cursor_index);
				}

				(KeyCode::Home, _) => { cursor_index = 0; }
				(KeyCode::End, _) => { cursor_index = filter_string.len(); }

				(KeyCode::Left, _) => { cursor_index = cursor_index.saturating_sub(1); }
				(KeyCode::Right, _) => { cursor_index += 1; }

				(KeyCode::Up, _) => { selected_index = selected_index.saturating_sub(1); }
				(KeyCode::Down, _) => { selected_index += 1; }
				(KeyCode::PageUp, _) => { selected_index = selected_index.saturating_sub(5); }
				(KeyCode::PageDown, _) => { selected_index += 5; }

				(KeyCode::Char(ch), _) => if ch.is_ascii() {
					filter_string.insert(cursor_index, ch);
					cursor_index += 1;
				}

				_ => {}
			}

			_ => {}
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
		cursor_index = cursor_index.min(filter_string.len());

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


fn git<S>(args: impl IntoIterator<Item=S>) -> anyhow::Result<String>
	where S: AsRef<std::ffi::OsStr>
{
	let cmd_output = Command::new("git")
		.args(args)
		.output()?;

	if !cmd_output.status.success() {
		let stderr = std::str::from_utf8(&cmd_output.stderr)?;
		anyhow::bail!("{stderr}");
	}

	Ok(String::from_utf8(cmd_output.stdout)?)
}

fn git_list<S>(args: impl IntoIterator<Item=S>) -> anyhow::Result<Vec<String>>
	where S: AsRef<std::ffi::OsStr>
{
	git(args)?
		.lines()
		.map(String::from)
		.map(Ok)
		.collect()
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
