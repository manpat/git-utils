use std::io::{stdout, Stdout, Write};
use std::fmt::Display;

use crate::on_drop;

use crossterm::{
	*,
	event::*,
	style::Color,
};

use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;



struct ListItem<T> {
	display: String,
	value: T,
}

pub struct FilterableList<T> {
	items: Vec<ListItem<T>>,
	prompt_text: String,

	filtered_items: Vec<usize>,
	needs_refilter: bool,
}

impl<T> FilterableList<T> {
	pub fn new(prompt_text: impl Into<String>) -> Self {
		FilterableList {
			items: Vec::new(),
			prompt_text: prompt_text.into(),

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

		let mut viewport = InlineViewport::start(self.items.len() + 1)?;

		let mut selected_index = 0usize;
		let mut caret_index = 0usize;
		let mut offset = 0;
		let mut filter_string = String::new();

		let matcher = SkimMatcherV2::default();

		'main: loop {
			let max_visible_items = viewport.usable_height() as usize - 1;

			// Refilter
			if self.needs_refilter {
				self.needs_refilter = false;

				#[derive(Ord, PartialOrd, Eq, PartialEq)]
				struct ScoredItem {
					score: i64,
					original_index: usize,
				}

				let mut item_scores: Vec<ScoredItem> = self.items.iter().enumerate()
					.filter_map(|(index, item)| {
						matcher.fuzzy_match(&item.display, &filter_string)
							.map(|score| ScoredItem {
								score: -score,
								original_index: index,
							})
					})
					.collect();

				item_scores.sort();

				self.filtered_items = item_scores.into_iter()
					.map(|item| item.original_index)
					.collect();
			}

			// Keep indices in bounds
			caret_index = caret_index.min(filter_string.len());

			if !self.filtered_items.is_empty() {
				selected_index = selected_index.min(self.filtered_items.len() - 1);
			}

			// Make sure max number of items possible are visible.
			offset = offset.min(self.filtered_items.len().saturating_sub(max_visible_items));

			// Make sure selection is in view
			if selected_index >= offset + max_visible_items {
				offset = selected_index - max_visible_items + 1;
			} else if selected_index < offset {
				offset = selected_index;
			}

			viewport.draw(|mut ctx| {
				ctx.print(&self.prompt_text);
				ctx.print(&filter_string);

				// Render list.
				for (row, &item_index) in
					self.filtered_items.iter()
						.skip(offset).take(max_visible_items)
						.enumerate()
				{
					let filtered_index = row + offset;
					let is_selected = filtered_index == selected_index;
					if is_selected {
						ctx.set_fg_color(Color::Black);
						ctx.set_bg_color(Color::White);

						ctx.print_at("> ", row as u16 + 1, 0);
					}

					let text = &self.items[item_index].display;
					ctx.print_at(text, row as u16 + 1, 2);

					ctx.reset_color();
				}

				// Move visual cursor to caret position
				ctx.move_to(0, self.prompt_text.len() as u16 + caret_index as u16);
			});

			'events: loop {
				match event::read()? {
					Event::Key(KeyEvent{ code, modifiers, kind: KeyEventKind::Press, .. }) => {
						match (code, modifiers) {
							(KeyCode::Enter, _) if !self.filtered_items.is_empty() => break 'main,

							(KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Esc, _) => {
								anyhow::bail!("Cancelled")
							}

							// Note: ctrl+backspace produces ^h on my machine.
							(KeyCode::Backspace, KeyModifiers::CONTROL) | (KeyCode::Char('h'), KeyModifiers::CONTROL) => {
								// Not quite right but whatever
								filter_string.clear();
								caret_index = 0;
								self.needs_refilter = true;
							}

							(KeyCode::Backspace, _) => if let Some(index) = caret_index.checked_sub(1) {
								filter_string.remove(index);
								caret_index -= 1;
								self.needs_refilter = true;
							}

							(KeyCode::Delete, _) => if !filter_string.is_empty() {
								filter_string.remove(caret_index);
								self.needs_refilter = true;
							}

							(KeyCode::Home, _) => { caret_index = 0; }
							(KeyCode::End, _) => { caret_index = filter_string.len(); }

							(KeyCode::Left, _) => { caret_index = caret_index.saturating_sub(1); }
							(KeyCode::Right, _) => { caret_index += 1; }

							(KeyCode::Up, _) => { selected_index = selected_index.saturating_sub(1); }
							(KeyCode::Down, _) => { selected_index += 1; }

							(KeyCode::PageUp, KeyModifiers::CONTROL) => { selected_index = 0; }
							(KeyCode::PageDown, KeyModifiers::CONTROL) => { selected_index = self.filtered_items.len(); }

							(KeyCode::PageUp, _) => { selected_index = selected_index.saturating_sub(max_visible_items); }
							(KeyCode::PageDown, _) => { selected_index += max_visible_items; }

							(KeyCode::Char(ch), _) => if ch.is_ascii() {
								filter_string.insert(caret_index, ch);
								caret_index += 1;
								self.needs_refilter = true;
							}

							_ => {}
						}

						break 'events
					}

					Event::Resize(width, height) => {
						viewport.terminal_width = width;
						viewport.terminal_height = height;
						break 'events
					}

					_ => {}
				}
			}
		}

		anyhow::ensure!(selected_index < self.filtered_items.len());

		let item_index = self.filtered_items[selected_index];
		Ok(self.items.remove(item_index).value)
	}
}







pub struct ViewportDrawContext {
	pub out: Stdout,

	pub start_row: u16,
	pub usable_width: u16,
	pub usable_height: u16,
}

impl Drop for ViewportDrawContext {
	fn drop(&mut self) {
		let _ = self.out.execute(terminal::EndSynchronizedUpdate);
	}
}

impl ViewportDrawContext {
	// pub fn flush(&mut self) {
	// 	self.out.flush().unwrap();
	// }

	pub fn print(&mut self, s: impl AsRef<str>) {
		self.out.queue(style::Print(s.as_ref())).unwrap();
	}

	pub fn print_at(&mut self, s: impl AsRef<str>, row: u16, column: u16) {
		self.move_to(row, column);
		self.out.queue(style::Print(s.as_ref())).unwrap();
	}

	pub fn move_to(&mut self, row: u16, column: u16) {
		self.out.queue(cursor::MoveTo(column, self.start_row + row)).unwrap();
	}

	pub fn set_fg_color(&mut self, color: Color) {
		self.out.queue(style::SetForegroundColor(color)).unwrap();
	}

	pub fn set_bg_color(&mut self, color: Color) {
		self.out.queue(style::SetBackgroundColor(color)).unwrap();
	}

	pub fn reset_color(&mut self) {
		self.out.queue(style::ResetColor).unwrap();
	}

	// pub fn cursor_column(&mut self) -> u16 {
	// 	self.flush();
	// 	cursor::position().unwrap().0
	// }

	// pub fn cursor_row(&mut self) -> u16 {
	// 	self.flush();
	// 	cursor::position().unwrap().1 - self.start_row
	// }
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

		terminal::enable_raw_mode()?;

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

	pub fn draw(&mut self, f: impl FnOnce(ViewportDrawContext)) {
		let _ = queue!{
			self.out,
			terminal::BeginSynchronizedUpdate,

			cursor::MoveTo(0, self.start_row),
			terminal::Clear(terminal::ClearType::FromCursorDown),
		};

		f(ViewportDrawContext {
			out: stdout(),

			start_row: self.start_row,
			usable_width: self.terminal_width,
			usable_height: self.usable_height(),
		});
	}
}


impl Drop for InlineViewport {
	fn drop(&mut self) {
		let _ = terminal::disable_raw_mode();
		let _ = execute!{
			self.out,
			cursor::MoveTo(0, self.start_row),
			terminal::Clear(terminal::ClearType::FromCursorDown),
			style::ResetColor,
			terminal::EnableLineWrap,
		};
	}
}

