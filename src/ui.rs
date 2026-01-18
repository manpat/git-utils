use std::io::{stdout, Stdout};
use std::fmt::Display;

use crate::on_drop;

use crossterm::{
	*,
	tty::*,
	event::*,
};

use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;



struct ListItem<T> {
	display: String,
	value: T,
}

pub struct FilterableList<T> {
	items: Vec<ListItem<T>>,

	filtered_items: Vec<usize>,
	needs_refilter: bool,
}

impl<T> FilterableList<T> {
	pub fn new() -> Self {
		FilterableList {
			items: Vec::new(),

			filtered_items: Vec::new(),
			needs_refilter: false,
		}
	}

	pub fn insert(&mut self, display: impl Into<String>, value: T) {
		self.items.push(ListItem { display: display.into(), value });
		self.needs_refilter = true;
	}

	pub fn insert_formatted(&mut self, value: T)
		where T: Display
	{
		self.insert(value.to_string(), value);
	}
}





impl<T> FilterableList<T> {
	pub fn run(mut self) -> anyhow::Result<T> {
		anyhow::ensure!(!self.items.is_empty());

		let mut out = stdout();

		let mut selected_index = 0usize;
		let mut caret_index = 0usize;
		let mut offset = 0;
		let mut filter_string = String::new();

		let (_, mut terminal_height) = terminal::size()?;
		let desired_height = terminal_height.min(self.items.len() as u16 + 1);
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

		#[derive(Ord, PartialOrd, Eq, PartialEq)]
		struct FilteredItem<'s> {
			score: i64,
			original_index: usize,
			text: &'s str,
		}

		let mut filtered_items: Vec<_> = self.items.iter().enumerate()
			.map(|(index, item)| FilteredItem {
				score: 0,
				original_index: index,
				text: item.display.as_str(),
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
						let desired_height = terminal_height.min(self.items.len() as u16 + 1);
						max_visible_items = desired_height as usize - 1;
						break 'events
					}

					_ => {}
				}
			}

			// Refilter
			filtered_items.clear();
			filtered_items.extend(
				self.items.iter().enumerate()
					.filter_map(|(index, item)| {
						matcher.fuzzy_match(&item.display, &filter_string)
							.map(|score| FilteredItem {
								score: -score,
								original_index: index,
								text: item.display.as_str(),
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

		let item_index = filtered_items[selected_index].original_index;
		Ok(self.items.remove(item_index).value)
	}
}










pub struct InlineViewport {
	out: Stdout,

	terminal_width: u16,
	terminal_height: u16,
	desired_height: usize,

	start_row: u16,
}

impl InlineViewport {
	pub fn start(desired_height: usize) -> anyhow::Result<Self> {
		let mut out = stdout();

		let (terminal_width, terminal_height) = terminal::size()?;
		let usable_height = (terminal_height as usize).min(desired_height);

		// Clear enough space
		{
			let num_newlines = usable_height.saturating_sub(1);
			for _ in 0..num_newlines { print!("\n"); }
			execute!{out, cursor::MoveUp(num_newlines as u16)}?;
		}

		execute!{
			out,
			terminal::DisableLineWrap,
		}?;

		let start_row = cursor::position()?.1;

		Ok(InlineViewport {
			out,

			terminal_width,
			terminal_height,
			desired_height,

			start_row,
		})
	}

	pub fn end(self) {}

	pub fn usable_height(&self) -> u16 {
		(self.terminal_height as usize).min(self.desired_height) as u16
	}
}


impl Drop for InlineViewport {
	fn drop(&mut self) {
		let _ = execute!{
			self.out,
			cursor::MoveTo(0, self.start_row),
			terminal::Clear(terminal::ClearType::FromCursorDown),
			style::ResetColor,
			terminal::EnableLineWrap,
		};
	}
}






fn start_raw_mode() -> anyhow::Result<impl Drop> {
	terminal::enable_raw_mode()?;
	Ok(on_drop(|| {
		terminal::disable_raw_mode().unwrap()
	}))
}
