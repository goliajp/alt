//! The arguments of `alt flow <kind> finish`.

use std::path::PathBuf;

#[derive(alt_args::Args)]
pub struct Finish {
    #[arg(hint = "flow-topic")]
    pub name: String,
    /// Tag the commit the finish leaves on its target, though the kind
    /// of topic is not tagged of itself
    #[arg(long = "tag", conflicts_with = "no_tag")]
    pub tag_made: bool,
    /// Make no tag
    #[arg(short = 'n', long = "notag")]
    pub no_tag: bool,
    /// The name of the tag, in place of the topic's own
    #[arg(short = 'T', long = "tagname", value_name = "NAME", hint = none)]
    pub tag_name: Option<String>,
    /// The message of the tag
    #[arg(short = 'm', long = "message", value_name = "MESSAGE", hint = none, conflicts_with = "tag_message_file")]
    pub tag_message: Option<String>,
    /// A file whose contents are the message of the tag
    #[arg(long = "messagefile", value_name = "FILE", hint = path)]
    pub tag_message_file: Option<PathBuf>,
}
