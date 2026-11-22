set -eu
id=$1 action=$2
case "$HOME" in /*) ;; *) exit 1 ;; esac
lock=$HOME/.minarbot-guide-install
[ -d "$lock" ] || exit 0
exec 9<"$lock/active"
flock -n 9
[ "$(cat "$lock/owner")" = "$id" ] || exit 1
if [ "$action" = rollback ]; then
    unit=$HOME/.config/systemd/user/minarbot.service
    if [ -f "$unit" ]; then
        systemctl --user disable --now minarbot.service
        rm -- "$unit"
        systemctl --user daemon-reload
    fi
fi
if [ -f "$lock/install" ]; then rm -- "$lock/install"; fi
rm -- "$lock/owner" "$lock/active"
rmdir "$lock"
