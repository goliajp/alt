# What the interactive zsh of the end-to-end tests starts with: the plugin,
# a key that prints the command line, and a key that prints what completion
# offers instead of offering it.

PS1='ready> '
unsetopt beep
autoload -Uz compinit
compinit -D -u
eval "$(alt shell zsh)"
zstyle ':completion:*' menu no

_test_show() {
  print -r -- "<buffer>$BUFFER</buffer>"
  BUFFER=
}
zle -N _test_show
bindkey '^Xs' _test_show

_test_offered() {
  compadd() {
    # a call that only asks which words match is passed on as it is
    if [[ ${@[1,(i)(-|--)]} == *-(O|A|D)\ * ]]; then
      builtin compadd "$@"
      return
    fi
    local -a hits
    local hit
    builtin compadd -A hits "$@"
    for hit in $hits; do
      print -r -- "<hit>$IPREFIX$hit</hit>"
    done
  }
  {
    _main_complete
  } always {
    unfunction compadd
  }
  print -r -- "<offered>"
}
zle -C _test_offered complete-word _test_offered
bindkey '^Xo' _test_offered
