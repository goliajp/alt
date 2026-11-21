# Drives an interactive zsh with the plugin loaded and prints, one line
# each, what it did: `zsh -f interactive.zsh <setup file> <repository>
# <slow repository> <quick repository>`.

zmodload zsh/zpty zsh/zselect zsh/datetime
setup=$1 repository=$2 slow=$3 quick=$4

zpty -b shell "cd ${(q)repository} && TERM=dumb zsh -f -i"

send() {
  zpty -w -n shell "$1"
}

# reads what the shell prints until it matches $1; REPLY is all of it
expect() {
  local chunk
  local -F deadline=$(( EPOCHREALTIME + 30 ))
  REPLY=
  while [[ $REPLY != ${~1} ]]; do
    if zpty -r shell chunk; then
      REPLY+=$chunk
    elif (( EPOCHREALTIME > deadline )); then
      print -r -- "timeout waiting for $1 after ${(qqq)REPLY}"
      exit 1
    else
      zselect -t 2
    fi
  done
}

# the command line after $1 is typed and Tab pressed
completed() {
  send "$1"$'\t\C-xs'
  expect '*<buffer>*</buffer>*'
  REPLY=${${REPLY##*<buffer>}%%</buffer>*}
}

# what completion offers behind $1, in the order it is offered
offered() {
  send "$1"$'\C-xo'
  expect '*<offered>*'
  local -a hits
  hits=(${(f)"$(print -r -- "$REPLY" | tr -d '\r')"})
  hits=(${(M)hits:#*'<hit>'*'</hit>'*})
  hits=(${${hits##*<hit>}%%</hit>*})
  REPLY=${(j: :)hits}
  send $'\C-u'
}

send "source ${(q)setup}; print loaded-\$(( 40 + 2 ))"$'\n'
expect '*loaded-42*ready> *'

completed 'alt sw'
print -r -- "subcommand=[$REPLY]"
completed 'alt switch fea'
print -r -- "branch=[$REPLY]"
completed 'alt commit --ame'
print -r -- "option=[$REPLY]"
offered 'alt switch '
print -r -- "branches=[$REPLY]"
offered 'alt add '
print -r -- "paths=[$REPLY]"
offered 'alt push origin main:'
print -r -- "refspecs=[$REPLY]"

# the prompt comes at once with what was known, and again when the state
# has arrived
send "setopt prompt_subst; PS1='<\$(alt_prompt_info plain)> '"$'\n'
expect '*<main*> *'
print -r -- "prompt=[<${${REPLY##*<}%%> *}>]"
send "cd ${(q)slow}"$'\n'
expect '*<> *<slow*> *'
print -r -- "late=[<${${REPLY##*<}%%> *}>]"

# an answer that comes after the prompt has moved on is not shown: the
# slow repository is asked, then left before it answers
send $'\n'
send "cd ${(q)quick}"$'\n'
expect '*<quick*> *'
zselect -t 250
send 'print "now-<$alt_prompt[branch]>"'$'\n'
expect '*now-<*>*<*> *'
print -r -- "kept=[${${REPLY##*now-<}%%>*}]"

send $'exit\n'
zpty -d shell
