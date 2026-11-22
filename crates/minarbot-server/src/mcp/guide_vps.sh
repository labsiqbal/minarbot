set -eu
umask 077
id=$1 source=$2 origin=$3 port=$4
case "$HOME" in /*) ;; *) exit 1 ;; esac
root=$HOME/.local/share/minarbot
config=$HOME/.config/minarbot
unit=$HOME/.config/systemd/user/minarbot.service
lock=$HOME/.minarbot-guide-install
[ "$(uname -s)" = Linux ] || exit 1
command -v curl >/dev/null
command -v python3 >/dev/null
command -v flock >/dev/null
systemctl --user show-environment >/dev/null
mkdir "$lock" 2>/dev/null || exit 20
if [ -e "$root" ] || [ -L "$root" ] || [ -e "$config" ] || [ -L "$config" ] || [ -e "$unit" ] || [ -L "$unit" ] \
    || systemctl --user cat minarbot.service >/dev/null 2>&1; then
    rmdir "$lock"
    exit 20
fi
exec 9>"$lock/active"
flock -n 9
printf '%s\n' "$id" > "$lock/owner"
# Check loopback port before touching the install tree. Service checks again when binding.
python3 -c 'import socket,sys; s=socket.socket(); s.bind(("127.0.0.1",int(sys.argv[1])))' "$port"
# Keep owner token on stdin. Installer reads it before using stdin for embedded Python.
curl -qfsS --proto '=https' --connect-timeout 10 --max-time 30 --max-filesize 65536 \
    "$source/api/install/script" -o "$lock/install"
sh "$lock/install" --source "$source" --token-stdin --public-origin "$origin" --port "$port" --no-open
