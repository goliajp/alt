//! The built-in drivers: for each language, the lines of the pattern
//! that finds what a hunk belongs to, as git has them.

use super::Builtin;

pub(super) const BUILTIN: &[Builtin] = &[
    Builtin {
        name: "ada",
        ignore_case: true,
        lines: &[
            "!^(.*[ \t])?(is[ \t]+new|renames|is[ \t]+separate)([ \t].*)?$",
            "!^[ \t]*with[ \t].*$",
            "^[ \t]*((procedure|function)[ \t]+.*)$",
            "^[ \t]*((package|protected|task)[ \t]+.*)$",
        ],
    },
    Builtin {
        name: "bash",
        ignore_case: false,
        lines: &[
            "^[ \t]*((([a-zA-Z_][a-zA-Z0-9_]*[ \t]*\\([ \t]*\\))|(function[ \t]+[a-zA-Z_][a-zA-Z0-9_]*(([ \t]*\\([ \t]*\\))|([ \t]+)))).*$)",
        ],
    },
    Builtin {
        name: "bibtex",
        ignore_case: false,
        lines: &["(@[a-zA-Z]{1,}[ \t]*\\{{0,1}[ \t]*[^ \t\"@',\\#}{~%]*).*$"],
    },
    Builtin {
        name: "cpp",
        ignore_case: false,
        lines: &[
            "!^[ \t]*[A-Za-z_][A-Za-z_0-9]*:[[:space:]]*($|/[/*])",
            "^((::[[:space:]]*)?[A-Za-z_].*)$",
        ],
    },
    Builtin {
        name: "csharp",
        ignore_case: false,
        lines: &[
            "!(^|[ \t]+)(do|while|for|foreach|if|else|new|default|return|switch|case|throw|catch|using|lock|fixed)([ \t(]+|$)",
            "^[ \t]*(([][[:alnum:]@_.](<[][[:alnum:]@_, \t<>]+>)?)+([ \t]+([][[:alnum:]@_.](<[][[:alnum:]@_, \t<>]+>)?)+)+[ \t]*\\([^;]*)$",
            "^[ \t]*(([][[:alnum:]@_.](<[][[:alnum:]@_, \t<>]+>)?)+([ \t]+([][[:alnum:]@_.](<[][[:alnum:]@_, \t<>]+>)?)+)+[^;=:,()]*)$",
            "^[ \t]*(((static|public|internal|private|protected|new|unsafe|sealed|abstract|partial)[ \t]+)*(class|enum|interface|struct|record)[ \t]+.*)$",
            "^[ \t]*(namespace[ \t]+.*)$",
        ],
    },
    Builtin {
        name: "css",
        ignore_case: true,
        lines: &["![:;][[:space:]]*$", "^[:[@.#]?[_a-z0-9].*$"],
    },
    Builtin {
        name: "dts",
        ignore_case: false,
        lines: &["!;", "!=", "^[ \t]*((/[ \t]*\\{|&?[a-zA-Z_]).*)"],
    },
    Builtin {
        name: "elixir",
        ignore_case: false,
        lines: &["^[ \t]*((def(macro|module|impl|protocol|p)?|test)[ \t].*)$"],
    },
    Builtin {
        name: "fortran",
        ignore_case: true,
        lines: &[
            "!^([C*]|[ \t]*!)",
            "!^[ \t]*MODULE[ \t]+PROCEDURE[ \t]",
            "^[ \t]*((END[ \t]+)?(PROGRAM|MODULE|BLOCK[ \t]+DATA|([^!'\" \t]+[ \t]+)*(SUBROUTINE|FUNCTION))[ \t]+[A-Z].*)$",
        ],
    },
    Builtin {
        name: "fountain",
        ignore_case: true,
        lines: &["^((\\.[^.]|(int|ext|est|int\\.?/ext|i/e)[. ]).*)$"],
    },
    Builtin {
        name: "golang",
        ignore_case: false,
        lines: &[
            "^[ \t]*(func[ \t]*.*(\\{[ \t]*)?)",
            "^[ \t]*(type[ \t].*(struct|interface)[ \t]*(\\{[ \t]*)?)",
        ],
    },
    Builtin {
        name: "html",
        ignore_case: false,
        lines: &["^[ \t]*(<[Hh][1-6]([ \t].*)?>.*)$"],
    },
    Builtin {
        name: "ini",
        ignore_case: false,
        lines: &["^[ \t]*\\[[^]]+\\]"],
    },
    Builtin {
        name: "java",
        ignore_case: false,
        lines: &[
            "!^[ \t]*(catch|do|for|if|instanceof|new|return|switch|throw|while)",
            "^[ \t]*(([a-z-]+[ \t]+)*(class|enum|interface|record)[ \t]+.*)$",
            "^[ \t]*(([A-Za-z_<>&][][?&<>.,A-Za-z_0-9]*[ \t]+)+[A-Za-z_][A-Za-z_0-9]*[ \t]*\\([^;]*)$",
        ],
    },
    Builtin {
        name: "kotlin",
        ignore_case: false,
        lines: &["^[ \t]*(([a-z]+[ \t]+)*(fun|class|interface)[ \t]+.*)$"],
    },
    Builtin {
        name: "markdown",
        ignore_case: false,
        lines: &["^ {0,3}#{1,6}[ \t].*"],
    },
    Builtin {
        name: "matlab",
        ignore_case: false,
        lines: &["^[[:space:]]*((classdef|function)[[:space:]].*)$|^(%%%?|##)[[:space:]].*$"],
    },
    Builtin {
        name: "objc",
        ignore_case: false,
        lines: &[
            "!^[ \t]*(do|for|if|else|return|switch|while)",
            "^[ \t]*([-+][ \t]*\\([ \t]*[A-Za-z_][A-Za-z_0-9* \t]*\\)[ \t]*[A-Za-z_].*)$",
            "^[ \t]*(([A-Za-z_][A-Za-z_0-9]*[ \t]+)+[A-Za-z_][A-Za-z_0-9]*[ \t]*\\([^;]*)$",
            "^(@(implementation|interface|protocol)[ \t].*)$",
        ],
    },
    Builtin {
        name: "pascal",
        ignore_case: false,
        lines: &[
            "^(((class[ \t]+)?(procedure|function)|constructor|destructor|interface|implementation|initialization|finalization)[ \t]*.*)$",
            "^(.*=[ \t]*(class|record).*)$",
        ],
    },
    Builtin {
        name: "perl",
        ignore_case: false,
        lines: &[
            "^package .*",
            "^sub [[:alnum:]_':]+[ \t]*(\\([^)]*\\)[ \t]*)?(:[^;#]*)?(\\{[ \t]*)?(#.*)?$",
            "^(BEGIN|END|INIT|CHECK|UNITCHECK|AUTOLOAD|DESTROY)[ \t]*(\\{[ \t]*)?(#.*)?$",
            "^=head[0-9] .*",
        ],
    },
    Builtin {
        name: "php",
        ignore_case: false,
        lines: &[
            "^[\t ]*(((public|protected|private|static|abstract|final)[\t ]+)*function.*)$",
            "^[\t ]*((((final|abstract)[\t ]+)?class|enum|interface|trait).*)$",
        ],
    },
    Builtin {
        name: "python",
        ignore_case: false,
        lines: &["^[ \t]*((class|(async[ \t]+)?def)[ \t].*)$"],
    },
    Builtin {
        name: "r",
        ignore_case: false,
        lines: &["^[ \t]*([a-zA-z][a-zA-Z0-9_.]*[ \t]*(<-|=)[ \t]*function.*)$"],
    },
    Builtin {
        name: "ruby",
        ignore_case: false,
        lines: &["^[ \t]*((class|module|def)[ \t].*)$"],
    },
    Builtin {
        name: "rust",
        ignore_case: false,
        lines: &[
            "^[\t ]*((pub(\\([^\\)]+\\))?[\t ]+)?((async|const|unsafe|extern([\t ]+\"[^\"]+\"))[\t ]+)?(struct|enum|union|mod|trait|fn|impl|macro_rules!)[< \t]+[^;]*)$",
        ],
    },
    Builtin {
        name: "scheme",
        ignore_case: false,
        lines: &[
            "^(\\(.*)$",
            "^[\t ]*(\\(((define|def(struct|syntax|class|method|rules|record|proto|alias)?)[-*/ \t]|(library|module|struct|class)[*+ \t]).*)$",
            "^  ?(\\([Dd][Ee][Ff].*)$",
        ],
    },
    Builtin {
        name: "tex",
        ignore_case: false,
        lines: &["^(\\\\((sub)*section|chapter|part)\\*{0,1}\\{.*)$"],
    },
];
