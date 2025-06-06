pub mod containers;
pub mod icc;

use clap::Subcommand;
pub use containers::ContainerCommands;
pub use icc::IccCommands;

#[derive(Subcommand, Debug)]
pub enum Commands {
    #[clap(subcommand)]
    Containers(ContainerCommands),

    #[clap(subcommand)]
    Icc(IccCommands),
} 