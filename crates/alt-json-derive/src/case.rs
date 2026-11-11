//! The `rename_all` rules. Field names are taken to be snake_case and
//! variant names PascalCase, as the language conventions have them.

use crate::Error;
use proc_macro::Span;

#[derive(Clone, Copy)]
pub(crate) enum Rule {
    Lower,
    Upper,
    Pascal,
    Camel,
    Snake,
    ScreamingSnake,
    Kebab,
    ScreamingKebab,
}

impl Rule {
    pub(crate) fn parse(name: &str, span: Span) -> Result<Self, Error> {
        Ok(match name {
            "lowercase" => Rule::Lower,
            "UPPERCASE" => Rule::Upper,
            "PascalCase" => Rule::Pascal,
            "camelCase" => Rule::Camel,
            "snake_case" => Rule::Snake,
            "SCREAMING_SNAKE_CASE" => Rule::ScreamingSnake,
            "kebab-case" => Rule::Kebab,
            "SCREAMING-KEBAB-CASE" => Rule::ScreamingKebab,
            _ => {
                return Err(Error::new(
                    span,
                    format!(
                        "unknown rename rule `{name}`, expected one of lowercase, UPPERCASE, \
                         PascalCase, camelCase, snake_case, SCREAMING_SNAKE_CASE, kebab-case, \
                         SCREAMING-KEBAB-CASE"
                    ),
                ));
            }
        })
    }

    pub(crate) fn variant(self, name: &str) -> String {
        match self {
            Rule::Pascal => name.to_owned(),
            Rule::Lower => name.to_ascii_lowercase(),
            Rule::Upper => name.to_ascii_uppercase(),
            Rule::Camel => name[..1].to_ascii_lowercase() + &name[1..],
            Rule::Snake => {
                let mut snake = String::new();
                for (i, c) in name.char_indices() {
                    if i > 0 && c.is_uppercase() {
                        snake.push('_');
                    }
                    snake.push(c.to_ascii_lowercase());
                }
                snake
            }
            Rule::ScreamingSnake => Rule::Snake.variant(name).to_ascii_uppercase(),
            Rule::Kebab => Rule::Snake.variant(name).replace('_', "-"),
            Rule::ScreamingKebab => Rule::ScreamingSnake.variant(name).replace('_', "-"),
        }
    }

    pub(crate) fn field(self, name: &str) -> String {
        match self {
            Rule::Lower | Rule::Snake => name.to_owned(),
            Rule::Upper | Rule::ScreamingSnake => name.to_ascii_uppercase(),
            Rule::Pascal => {
                let mut pascal = String::new();
                let mut capitalize = true;
                for c in name.chars() {
                    if c == '_' {
                        capitalize = true;
                    } else if capitalize {
                        pascal.push(c.to_ascii_uppercase());
                        capitalize = false;
                    } else {
                        pascal.push(c);
                    }
                }
                pascal
            }
            Rule::Camel => {
                let pascal = Rule::Pascal.field(name);
                pascal[..1].to_ascii_lowercase() + &pascal[1..]
            }
            Rule::Kebab => name.replace('_', "-"),
            Rule::ScreamingKebab => Rule::ScreamingSnake.field(name).replace('_', "-"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Rule;

    const RULES: [Rule; 8] = [
        Rule::Lower,
        Rule::Upper,
        Rule::Pascal,
        Rule::Camel,
        Rule::Snake,
        Rule::ScreamingSnake,
        Rule::Kebab,
        Rule::ScreamingKebab,
    ];

    #[test]
    fn variants() {
        let names = |name: &str| RULES.map(|rule| rule.variant(name));
        assert_eq!(
            names("TaskBundlesV1"),
            [
                "taskbundlesv1",
                "TASKBUNDLESV1",
                "TaskBundlesV1",
                "taskBundlesV1",
                "task_bundles_v1",
                "TASK_BUNDLES_V1",
                "task-bundles-v1",
                "TASK-BUNDLES-V1"
            ]
        );
        assert_eq!(names("HTTP")[4], "h_t_t_p");
        assert_eq!(names("A")[3], "a");
    }

    #[test]
    fn fields() {
        let names = |name: &str| RULES.map(|rule| rule.field(name));
        assert_eq!(
            names("last_login_at"),
            [
                "last_login_at",
                "LAST_LOGIN_AT",
                "LastLoginAt",
                "lastLoginAt",
                "last_login_at",
                "LAST_LOGIN_AT",
                "last-login-at",
                "LAST-LOGIN-AT"
            ]
        );
        assert_eq!(names("x")[3], "x");
        assert_eq!(names("id2_of")[2], "Id2Of");
    }
}
