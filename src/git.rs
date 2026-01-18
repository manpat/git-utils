use std::process::{self, Command, ExitStatus};
use std::path::PathBuf;

pub struct GitContext {
	working_dir: Option<PathBuf>,
}

impl GitContext {
	pub fn new(args: &crate::MainArgs) -> GitContext {
		GitContext {
			working_dir: args.working_dir.clone(),
		}
	}

	pub fn run_raw<S>(&self, args: impl IntoIterator<Item=S>) -> anyhow::Result<GitOutput>
		where S: AsRef<std::ffi::OsStr>
	{
		let args: Vec<_> = args.into_iter().collect();
		let arg_strings: Vec<_> = args.iter().map(AsRef::as_ref).collect();

		log::info!("> git {arg_strings:?}");

		let mut command = Command::new("git");
		command.args(args);

		if let Some(dir) = self.working_dir.as_ref() {
			command.current_dir(dir);
		}

		let process::Output{ status, stdout, stderr } = command.output()?;

		log::info!(" -> status: {status:?}");

		let stdout = std::str::from_utf8(&stdout)?.trim().to_owned();
		let stderr = std::str::from_utf8(&stderr)?.trim().to_owned();

		Ok(GitOutput {
			status,
			stdout,
			stderr,
		})
	}

	pub fn query<S>(&self, args: impl IntoIterator<Item=S>) -> anyhow::Result<String>
		where S: AsRef<std::ffi::OsStr>
	{
		let GitOutput{status, stdout, stderr} = self.run_raw(args)?;

		if !status.success() {
			log::error!("{stderr}");
			anyhow::bail!("{stderr}");
		}

		Ok(stdout)
	}

	pub fn try_query<S>(&self, args: impl IntoIterator<Item=S>) -> anyhow::Result<Option<String>>
		where S: AsRef<std::ffi::OsStr>
	{
		let GitOutput{status, stdout, stderr} = self.run_raw(args)?;
		match status.code() {
			Some(0) => Ok(Some(stdout)),
			Some(1) => Ok(None),
			_ => anyhow::bail!("{stderr}"),
		}
	}

	pub fn query_list<S>(&self, args: impl IntoIterator<Item=S>) -> anyhow::Result<Vec<String>>
		where S: AsRef<std::ffi::OsStr>
	{
		self.query(args)?
			.lines()
			.map(String::from)
			.map(Ok)
			.collect()
	}

	pub fn query_success<S>(&self, args: impl IntoIterator<Item=S>) -> anyhow::Result<bool>
		where S: AsRef<std::ffi::OsStr>
	{
		self.try_query(args)
			.map(|result| result.is_some())
	}

	pub fn run<S>(&self, args: impl IntoIterator<Item=S>) -> anyhow::Result<()>
		where S: AsRef<std::ffi::OsStr>
	{
		self.query(args)
			.map(|_| ())
	}
}



pub struct GitOutput {
	pub status: ExitStatus,
	pub stdout: String,
	pub stderr: String,
}