#!/usr/bin/env bash
# Disposable real IM/PG/Redis fixture; never reuses or resets a user's database.
set -euo pipefail
umask 077
repo=$(cd "$(dirname "$0")/../.." && pwd)
binary="${PRIVCHAT_FEATURE_BINARY:-$repo/target/debug/privchat}"
base="${PRIVCHAT_FEATURE_BASE_PORT:-58410}"
[[ "$base" =~ ^[0-9]+$ ]] && (( base >= 30000 && base <= 60000 ))
for offset in 0 1 2 3 4 5; do
  if lsof -nP -iTCP:$((base+offset)) -sTCP:LISTEN -t >/dev/null 2>&1; then
    echo "Fixture port is in use; choose another base, never stop the owner" >&2; exit 1
  fi
done
if lsof -nP -iUDP:$((base+2)) -t >/dev/null 2>&1; then
  echo "Fixture QUIC port is in use; choose another base, never stop the owner" >&2; exit 1
fi
parent="${PRIVCHAT_FEATURE_PARENT:-$repo/.artifacts/feature-acceptance}"
[[ "$parent" = /* && "$parent" != / && ! -L "$parent" ]] || exit 2
mkdir -p "$parent"
chmod 700 "$parent"
root=$(mktemp -d "$parent/privchat-features-im.XXXXXX")
chmod 700 "$root"
trap 'status=$?; echo "Fixture step failed ($status); private diagnostics: $root" >&2; exit "$status"' ERR
cp -p "$binary" "$root/server-bin"
shasum -a 256 "$root/server-bin" > "$root/binary.sha256"
binary="$root/server-bin"
database="privchat_features_$(date +%Y%m%d%H%M%S)_$(basename "$root" | tr '[:upper:].-' '[:lower:]__')"
psql="${PSQL:-/opt/homebrew/opt/postgresql@16/bin/psql}"
"$psql" postgres -X -v ON_ERROR_STOP=1 -c "CREATE DATABASE $database" > "$root/create.log"
printf '%s\n' "$database" > "$root/db-name"
export FEATURE_ROOT="$root" FEATURE_BASE="$base" FEATURE_DATABASE="$database"
(cd "$root" && "$binary" generate-config "$root/config.toml") > "$root/generate.log" 2>&1
node - <<'JS'
const fs=require('node:fs'), path=require('node:path');
const root=process.env.FEATURE_ROOT, base=Number(process.env.FEATURE_BASE);
let text=fs.readFileSync(path.join(root,'config.toml'),'utf8');
function key(section,name,value) {
  const escaped=section.replace(/[.*+?^${}()|[\]\\]/g,'\\$&');
  const re=new RegExp('(^\\['+escaped+'\\]\\s*\\n)([\\s\\S]*?)(?=^\\[|$(?![\\s\\S]))','m');
  if(!re.test(text)) throw Error('Missing config section '+section);
  text=text.replace(re,(_,head,body)=>head+(new RegExp('^'+name+'\\s*=.*$','m').test(body)
    ?body.replace(new RegExp('^'+name+'\\s*=.*$','m'),name+' = '+value):name+' = '+value+'\n'+body));
}
text=text.replace(/\[\[gateway\.listeners\]\][\s\S]*?(?=^\[|$(?![\s\S]))/gm,'');
text+=`\n[[gateway.listeners]]\nprotocol = "tcp"\nhost = "127.0.0.1"\nport = ${base+1}\n\n[[gateway.listeners]]\nprotocol = "websocket"\nhost = "127.0.0.1"\nport = ${base}\npath = "/gate"\n\n[[gateway.listeners]]\nprotocol = "quic"\nhost = "127.0.0.1"\nport = ${base+2}\n`;
key('database','url',JSON.stringify('postgres://'+process.env.USER+'@localhost/'+process.env.FEATURE_DATABASE));
key('gateway.tls','cert',JSON.stringify(path.join(root,'certs/server.crt')));
key('gateway.tls','key',JSON.stringify(path.join(root,'certs/server.key')));
key('cache.redis','url',JSON.stringify(`redis://127.0.0.1:${base+5}`));
key('admin','host','"127.0.0.1"');key('admin','port',String(base+4));
key('file','server_host','"127.0.0.1"');key('file','server_port',String(base+3));
key('file','server_api_base_url',JSON.stringify(`http://127.0.0.1:${base+3}/api/app`));
text=text.replace(/^storage_root\s*=.*$/gm,'storage_root = '+JSON.stringify(path.join(root,'storage')));
key('logging','file',JSON.stringify(path.join(root,'logs/server.log')));
key('system_message','auto_create_channel','false');key('system_message','auto_send_welcome','false');
key('call','enabled','true');key('call.peer','enabled','true');key('call.peer','allow_turn','false');
fs.writeFileSync(path.join(root,'config.toml'),text,{mode:0o600});
fs.writeFileSync(path.join(root,'public.json'),JSON.stringify({root,database:process.env.FEATURE_DATABASE,host:'127.0.0.1',wsPort:base,tcpPort:base+1,quicPort:base+2,filePort:base+3,adminPort:base+4,redisPort:base+5},null,2));
JS
(cd "$root" && DATABASE_URL="postgres://$USER@localhost/$database" "$binary" migrate) > "$root/migrate.log" 2>&1
redis-server --bind 127.0.0.1 --port "$((base+5))" --save '' --appendonly no --daemonize yes --pidfile "$root/redis.pid" --logfile "$root/redis.log"
openssl rand -hex 32 > "$root/master-key"
FEATURE_BINARY="$binary" node - <<'JS'
const fs=require('node:fs'), {spawn}=require('node:child_process');
const root=process.env.FEATURE_ROOT, base=Number(process.env.FEATURE_BASE);
const fd=fs.openSync(root+'/server.log','a',0o600);
const child=spawn(process.env.FEATURE_BINARY,['--config-file',root+'/config.toml','--env','test'],{
  cwd:root,detached:true,stdio:['ignore',fd,fd],env:{...process.env,
    SERVICE_MASTER_KEY:fs.readFileSync(root+'/master-key','utf8').trim(),
    REDIS_URL:`redis://127.0.0.1:${base+5}`,
    DATABASE_URL:`postgres://${process.env.USER}@localhost/${process.env.FEATURE_DATABASE}`}});
fs.writeFileSync(root+'/launcher.pid',String(child.pid));child.unref();fs.closeSync(fd);
JS
for attempt in $(seq 1 60); do
  if lsof -nP -iTCP:"$base" -sTCP:LISTEN -t > "$root/server.pid" 2>/dev/null; then
    openssl x509 -in "$root/certs/server.crt" -pubkey -noout | openssl pkey -pubin -outform der | openssl dgst -sha256 -binary | openssl base64 -A > "$root/spki-pins.txt"
    chmod 600 "$root"/*.log "$root/spki-pins.txt"
    echo "$root"; exit 0
  fi
  sleep 1
done
echo "Fixture failed to listen; inspect private $root/server.log" >&2
exit 1
