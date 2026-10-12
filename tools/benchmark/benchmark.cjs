'use strict';
const fs=require('node:fs');
const path=require('node:path');
const crypto=require('node:crypto');
const os=require('node:os');
const net=require('node:net');
const http=require('node:http');
const {spawn}=require('node:child_process');
const readline=require('node:readline');
const {performance}=require('node:perf_hooks');
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
const json=(file,value)=>fs.writeFileSync(file,JSON.stringify(value,null,2));
const sha=bytes=>crypto.createHash('sha1').update(bytes).digest();

function encode(value){
 if(Buffer.isBuffer(value)||typeof value==='string'){const b=Buffer.isBuffer(value)?value:Buffer.from(value);return Buffer.concat([Buffer.from(b.length+':'),b]);}
 if(typeof value==='number')return Buffer.from('i'+value+'e');
 if(Array.isArray(value))return Buffer.concat([Buffer.from('l'),...value.map(encode),Buffer.from('e')]);
 return Buffer.concat([Buffer.from('d'),...Object.keys(value).sort().flatMap(k=>[encode(k),encode(value[k])]),Buffer.from('e')]);
}
function decode(bytes){
 let at=0,infoStart,infoEnd;
 function read(depth=0){
  if(depth>64)throw new Error('Bencode nesting too deep');
  const start=at,c=bytes[at++];
  if(c===105){const end=bytes.indexOf(101,at);if(end<0)throw new Error('Bad integer');const n=Number(bytes.subarray(at,end).toString());at=end+1;if(!Number.isSafeInteger(n))throw new Error('Bad integer');return n;}
  if(c===108||c===100){const v=c===108?[]:Object.create(null);while(bytes[at]!==101){if(at>=bytes.length)throw new Error('Truncated bencode');if(c===108)v.push(read(depth+1));else{const k=read(depth+1).toString();const from=at;v[k]=read(depth+1);if(depth===0&&k==='info'){infoStart=from;infoEnd=at;}}}at++;return v;}
  at=start;const colon=bytes.indexOf(58,at);if(colon<0)throw new Error('Bad string');const n=Number(bytes.subarray(at,colon).toString());if(!Number.isSafeInteger(n)||n<0||colon+1+n>bytes.length)throw new Error('Bad string length');at=colon+1+n;return bytes.subarray(colon+1,at);
 }
 const value=read();return{value,infoBytes:infoStart===undefined?undefined:bytes.subarray(infoStart,infoEnd)};
}
function torrentInfo(file){
 const bytes=fs.readFileSync(file),{value,infoBytes}=decode(bytes),info=value.info;
 if(!infoBytes||!info||!Buffer.isBuffer(info.pieces))throw new Error('Invalid torrent file');
 const name=(info['name.utf-8']||info.name).toString('utf8');
 const files=info.files?info.files.map(f=>({parts:[name,...(f['path.utf-8']||f.path).map(p=>p.toString('utf8'))],length:f.length})): [{parts:[name],length:info.length}];
 const total=files.reduce((n,f)=>n+f.length,0);
 if(!Number.isSafeInteger(total)||total<=0||info['piece length']<=0||info.pieces.length!==Math.ceil(total/info['piece length'])*20)throw new Error('Invalid torrent size or piece hashes');
 return{bytes,hash:sha(infoBytes).toString('hex'),name,files,total,pieceSize:info['piece length'],pieces:info.pieces};
}
async function verifyFiles(root,info){
 let piece=crypto.createHash('sha1'),used=0,index=0,total=0;
 for(const f of info.files){
  const target=path.resolve(root,...f.parts);
  if(!target.startsWith(path.resolve(root)+path.sep))throw new Error('Unsafe torrent file path');
  if(fs.statSync(target).size!==f.length)throw new Error('Downloaded file size differs: '+f.parts.join('/'));
  for await(const chunk of fs.createReadStream(target)){
   let at=0;while(at<chunk.length){const n=Math.min(info.pieceSize-used,chunk.length-at);piece.update(chunk.subarray(at,at+n));used+=n;total+=n;at+=n;
    if(used===info.pieceSize||total===info.total){if(!piece.digest().equals(info.pieces.subarray(index*20,index*20+20)))throw new Error('Piece hash mismatch: '+index);index++;piece=crypto.createHash('sha1');used=0;}
   }
  }
 }
 if(total!==info.total)throw new Error('Payload size mismatch');
 return{pieces:index,bytes:total,verified:true};
}
class Metrics{
 constructor(){this.process=spawn('powershell.exe',['-NoProfile','-File',path.join(__dirname,'metrics.ps1')],{windowsHide:true,stdio:['pipe','pipe','pipe']});this.waiting=[];this.stderr='';this.process.stderr.on('data',b=>this.stderr+=b);readline.createInterface({input:this.process.stdout}).on('line',line=>{const waiter=this.waiting.shift();if(waiter){try{waiter.resolve(JSON.parse(line));}catch(e){waiter.reject(e);}}});this.process.on('exit',()=>{for(const w of this.waiting)w.reject(new Error('Metrics helper exited: '+this.stderr));this.waiting=[];});}
 request(input){return new Promise((resolve,reject)=>{this.waiting.push({resolve,reject});this.process.stdin.write(JSON.stringify(input)+'\n');});}
 close(){this.process.stdin.end();}
}
async function freePort(){const server=net.createServer();await new Promise(r=>server.listen(0,'127.0.0.1',r));const port=server.address().port;await new Promise(r=>server.close(r));return port;}
async function waitFor(fn,child){for(let i=0;i<60;i++){if(child.exitCode!==null)throw new Error('Client exited before API was ready');try{return await fn();}catch{}await sleep(500);}throw new Error('Client API did not become ready in 30 seconds');}
async function request(url,options={}){const r=await fetch(url,{...options,signal:AbortSignal.timeout(5000)});if(!r.ok)throw new Error('HTTP '+r.status+' from localhost client');return r;}
async function stopChild(child){if(!child||child.exitCode!==null||child.signalCode!==null)return;child.kill();for(let i=0;i<25&&child.exitCode===null&&child.signalCode===null;i++)await sleep(200);if(child.exitCode===null&&child.signalCode===null)throw new Error('Test process did not stop');}

async function startTortuga(exe,dir,selfTest,cache=[]){
 const profile=path.join(dir,'profile'),downloads=path.join(dir,'download');fs.mkdirSync(profile,{recursive:true});fs.mkdirSync(downloads);
 json(path.join(profile,'settings.json'),{download_dir:downloads,network:{mode:'system'},utp:true,dht:!selfTest,lsd:!selfTest,upnp:!selfTest,peer_limit:128,download_limit:0,upload_limit:0,default_sequential:false,close_to_tray:false});
 for(const file of cache)fs.copyFileSync(file.source,path.join(profile,file.name));
 const apiFile=path.join(profile,'api-info.json'),out=fs.openSync(path.join(dir,'engine.log'),'w'),err=fs.openSync(path.join(dir,'stderr.log'),'w');
 const child=spawn(exe,['--data-dir',profile,'--api-info-file',apiFile],{windowsHide:true,env:{...process.env,RUST_LOG:'info,librqbit=warn'},stdio:['ignore',out,err]});fs.closeSync(out);fs.closeSync(err);
 try{
  const b=await waitFor(async()=>{const b=JSON.parse(fs.readFileSync(apiFile));if(!/^http:\/\/127\.0\.0\.1:\d+$/.test(b.base))throw new Error('Unexpected API address');await request(b.base+'/api/state',{headers:{'x-tortuga-token':b.token}});return b;},child);
  const call=async(method,endpoint,body)=>(await request(b.base+endpoint,{method,headers:{'x-tortuga-token':b.token,'content-type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)})).json();
  return{child,downloads,settings:await call('GET','/api/settings'),add:file=>call('POST','/api/torrents',{source:file,save_path:downloads,paused:false}),sample:async hash=>{const state=await call('GET','/api/state'),t=state.torrents.find(t=>t.id===hash);if(!t)throw new Error('Torrent is missing');return{done:t.state==='checking'?0:t.done,rawDone:t.done,downloaded:t.downloaded,uploaded:t.uploaded,downBps:t.down_speed,upBps:t.up_speed,peers:t.peers,seen:t.peers_seen,status:t.state,complete:t.done===t.size&&t.size>0&&t.state!=='checking',network:state.session.network};},peers:hash=>call('GET','/api/torrents/'+hash+'/peers'),details:hash=>call('GET','/api/torrents/'+hash),pause:hash=>call('POST','/api/torrents/'+hash+'/pause')};
 }catch(e){await stopChild(child);throw e;}
}
async function startUtorrent(exe,dir,selfTest,cache=[]){
 const profile=path.join(dir,'profile'),downloads=path.join(dir,'download');fs.mkdirSync(profile,{recursive:true});fs.mkdirSync(downloads);
 const portable=path.join(profile,'utorrent.exe');fs.copyFileSync(exe,portable);
 const port=await freePort(),password=crypto.randomBytes(20).toString('hex'),username='benchmark';
 const settings={'webui.enable':1,'webui.enable_listen':1,'webui.port':port,'webui.username':username,'webui.password':password,'webui.restrict':'127.0.0.1','dir_active_download':downloads,'dir_active_download_flag':1,'dir_completed_download_flag':0,'max_dl_rate':0,'max_ul_rate':0,'conns_per_torrent':128,'conns_globally':128,'dht':selfTest?0:1,'dht_per_torrent':selfTest?0:1,'lsd':selfTest?0:1,'upnp':selfTest?0:1,'natpmp':selfTest?0:1,'bt.allow_same_ip':selfTest?1:0,'check_updates':0,'install':0,'settings_saved_systime':Math.floor(Date.now()/1000)};
 fs.writeFileSync(path.join(profile,'settings.dat'),encode(settings));
 for(const file of cache)fs.copyFileSync(file.source,path.join(profile,file.name));
 const child=spawn(portable,['/NOINSTALL'],{cwd:profile,windowsHide:true,stdio:'ignore'}),base='http://127.0.0.1:'+port+'/gui/';
 try{
  const auth='Basic '+Buffer.from(username+':'+password).toString('base64');let cookie='';
  const token=await waitFor(async()=>{const r=await request(base+'token.html',{headers:{Authorization:auth}});cookie=r.headers.get('set-cookie')?.split(';')[0]||'';const text=await r.text(),token=text.match(/<div[^>]*id=['"]token['"][^>]*>([^<]+)<\/div>/i)?.[1];if(!token)throw new Error('uTorrent token missing');return token;},child);
  const call=async(params,body)=>{const query=new URLSearchParams({...params,token});return(await request(base+'?'+query,{method:body?'POST':'GET',headers:{Authorization:auth,Cookie:cookie},body})).json();};
  const response=await call({action:'getsettings'});const allowed=response.settings.filter(s=>/^(max_dl_rate|max_ul_rate|conns_|dht|dht_per_torrent|lsd|upnp|natpmp|bt\.transp_disposition|bt\.allow_same_ip|diskio\.|dir_active)/.test(s[0]));
  const configured=allowed.find(s=>s[0]==='dir_active_download')?.[2],enabled=allowed.find(s=>s[0]==='dir_active_download_flag')?.[2];if(!configured||path.resolve(configured)!==path.resolve(downloads)||enabled!=='true')throw new Error('uTorrent ignored its isolated download directory');
  return{child,downloads,settings:allowed,call,add:async file=>{const form=new FormData();form.append('torrent_file',new Blob([fs.readFileSync(file)]),path.basename(file));return call({action:'add-file'},form);},sample:async hash=>{const data=await call({list:'1'}),t=data.torrents?.find(t=>t[0].toLowerCase()===hash);if(!t)throw new Error('Torrent is missing');return{done:Math.round(t[3]*t[4]/1000),downloaded:t[5],uploaded:t[6],downBps:t[9],upBps:t[8],peers:t[12]+t[14],seen:t[13]+t[15],status:t[21]||String(t[1]),complete:t[4]>=1000,raw:t};},peers:hash=>call({action:'getpeers',hash}),details:hash=>call({action:'getprops',hash}),pause:hash=>call({action:'pause',hash})};
 }catch(e){await stopChild(child);throw e;}
}

async function runOne(clientName,round,options,metrics,info){
 const dir=path.join(options.output,`round-${round}-${clientName}`);fs.mkdirSync(dir);console.log(`\n${clientName}, round ${round}: ${info.name}`);
 const start=clientName==='Tortuga'?startTortuga:startUtorrent;
 const launched=performance.now();const client=await start(options[clientName],dir,options.selfTest,options.cache?.[clientName]||[]);const startupSeconds=(performance.now()-launched)/1000;json(path.join(dir,'settings.json'),client.settings);
 const samples=[],peerSnapshots=[],started=performance.now();let firstByte=null,previous=null,peak=0,verification=null,error=null,lastPeerAt=-10;
 const csv=fs.createWriteStream(path.join(dir,'samples.csv'));csv.write('elapsed_s,done_bytes,downloaded_bytes,uploaded_bytes,reported_down_bps,reported_up_bps,payload_down_bps,peers,seen,cpu_percent,working_set_bytes,process_io_read_bytes,process_io_write_bytes,status\n');
 try{
  await client.add(options.torrent);
  while(true){
   const at=performance.now(),elapsed=(at-started)/1000;
   if(elapsed>options.timeout)throw new Error('Download timed out');
   const t=await client.sample(info.hash),m=await metrics.request({pid:client.child.pid,connections:elapsed-lastPeerAt>=5});
   if(m.error)throw new Error(m.error);
   if((t.done>0||t.downloaded>0)&&firstByte===null)firstByte=elapsed;
   const dt=previous?elapsed-previous.elapsed:elapsed;
   const payload=Math.max(0,t.done-(previous?.done||0))/Math.max(dt,0.001),cpu=previous?Math.max(0,m.cpuMs-previous.metrics.cpuMs)/(dt*1000*os.cpus().length)*100:0;
   peak=Math.max(peak,payload);
   const sample={elapsed,...t,payloadBps:payload,metrics:m,cpuPercent:cpu};samples.push(sample);fs.appendFileSync(path.join(dir,'samples.jsonl'),JSON.stringify(sample)+'\n');
   csv.write([elapsed.toFixed(3),t.done,t.downloaded,t.uploaded,t.downBps,t.upBps,Math.round(payload),t.peers,t.seen,cpu.toFixed(3),m.rssBytes,m.ioReadBytes,m.ioWriteBytes,JSON.stringify(t.status)].join(',')+'\n');
   if(elapsed-lastPeerAt>=5){try{const p=await client.peers(info.hash);peerSnapshots.push({elapsed,peers:p});fs.appendFileSync(path.join(dir,'peers.jsonl'),JSON.stringify({elapsed,peers:p})+'\n');}catch(e){fs.appendFileSync(path.join(dir,'diagnostics.log'),'Peer snapshot: '+e.message+'\n');}lastPeerAt=elapsed;}
   if(samples.length%5===0||t.complete)console.log(`${elapsed.toFixed(1)} s · ${(t.done/info.total*100).toFixed(1)}% · ${(t.downBps/1048576).toFixed(2)} MiB/s · ${t.peers} peers`);
   if(t.complete){
    const seconds=(performance.now()-started)/1000;
    await client.pause(info.hash);json(path.join(dir,'details.json'),await client.details(info.hash));await stopChild(client.child);await sleep(500);
    verification=await verifyFiles(client.downloads,info);
    const row={client:clientName,round,seconds,startupSeconds,endToEndSeconds:startupSeconds+seconds,firstByteSeconds:firstByte,meanPayloadBps:info.total/seconds,meanEndToEndBps:info.total/(startupSeconds+seconds),meanTransferBps:firstByte===null?0:info.total/Math.max(0.001,seconds-firstByte),peakPayloadBps:peak,peakReportedBps:Math.max(...samples.map(s=>s.downBps)),meanPeers:samples.reduce((n,s)=>n+s.peers,0)/samples.length,meanCpuPercent:samples.reduce((n,s)=>n+s.cpuPercent,0)/samples.length,peakMemoryBytes:Math.max(...samples.map(s=>s.metrics.rssBytes)),verification,samples,peerSnapshots};
    json(path.join(dir,'result.json'),{...row,samples:undefined,peerSnapshots:undefined});return row;
   }
   previous=sample;await sleep(Math.max(0,1000-(performance.now()-at)));
  }
 }catch(e){error=e.message;json(path.join(dir,'error.json'),{client:clientName,round,error,samples:samples.length});throw e;}
 finally{csv.end();await stopChild(client.child);await metrics.request({root:options.output,stopHelpers:dir});await sleep(500);if(!options.keepDownloads&&verification){const result=await metrics.request({root:options.output,cleanup:client.downloads});if(result.error)throw new Error('Download cleanup failed: '+result.error);}const result=await metrics.request({root:options.output,cleanup:path.join(dir,'profile')});if(result.error)throw new Error('Profile cleanup failed: '+result.error);for(const file of ['engine.log','details.json']){const target=path.join(dir,file);if(fs.existsSync(target)){const text=fs.readFileSync(target,'utf8').replace(/https?:\/\/[^\s"'<>]+/g,url=>{try{const u=new URL(url);u.username='';u.password='';u.search='';return u.toString();}catch{return '[tracker-url]';}});fs.writeFileSync(target,text);}}}
}
function writeReport(output,rows,info,environment){
 const compact=rows.map(({samples,peerSnapshots,...r})=>r);json(path.join(output,'report.json'),{torrent:{name:info.name,hash:info.hash,total:info.total},environment,runs:compact});
 const grouped=name=>rows.filter(r=>r.client===name);const means=name=>grouped(name).reduce((n,r)=>n+r.meanPayloadBps,0)/grouped(name).length;
 const ratio=means('Tortuga')/means('uTorrent');const summary=Number.isFinite(ratio)?`Tortuga / uTorrent, mean full-download throughput: ${ratio.toFixed(2)}x`:'Comparison incomplete';
 const lines=[info.name,`Info hash: ${info.hash}`,`Payload: ${info.total} bytes`,summary,'','Client | Round | Seconds | Mean MiB/s | Peak reported MiB/s | Avg peers | Avg CPU % | Verified',...compact.map(r=>`${r.client} | ${r.round} | ${r.seconds.toFixed(2)} | ${(r.meanPayloadBps/1048576).toFixed(2)} | ${(r.peakReportedBps/1048576).toFixed(2)} | ${r.meanPeers.toFixed(1)} | ${r.meanCpuPercent.toFixed(2)} | ${r.verification.verified}`),'','Mean throughput = verified payload / full elapsed download time, including waiting for peers.','Maximum reported speed uses each client\'s own estimator and is not directly comparable.','samples.csv contains common payload rates calculated from progress deltas. uTorrent progress is rounded to 0.1%, which makes one-second rates noisy.','process_io_* is all process I/O (including sockets), not a physical-disk benchmark.','Different peer sets, protocol selection, warm DHT/trackers and network variation remain possible causes.','AB/BA reduces ordering bias; it does not prove a causal performance advantage.','Raw settings, peers, connections, engine logs and samples are kept next to this report. Credentials and downloaded films are excluded.'];
 fs.writeFileSync(path.join(output,'report.txt'),lines.join('\r\n'));
 const escape=s=>String(s).replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
 const duration=Math.max(...rows.map(r=>r.seconds),1),top=Math.max(...rows.flatMap(r=>r.samples.map(s=>s.downBps)),1);
 const chart=rows.map((r,i)=>`<polyline fill="none" stroke="${r.client==='Tortuga'?'#e6a635':'#72bc55'}" stroke-width="2" ${i>=2?'stroke-dasharray="5 3"':''} points="${r.samples.map(s=>`${(s.elapsed/duration*900).toFixed(1)},${(240-s.downBps/top*230).toFixed(1)}`).join(' ')}"/>`).join('');
 fs.writeFileSync(path.join(output,'report.html'),`<!doctype html><html lang="ru"><meta charset="utf-8"><title>Tortuga / uTorrent benchmark</title><style>body{font:15px system-ui;background:#121519;color:#e8eaed;max-width:1100px;margin:40px auto;padding:0 20px}table{border-collapse:collapse;width:100%}td,th{padding:10px;border-bottom:1px solid #343a43;text-align:left}svg{width:100%;height:260px;background:#191e24;border-radius:8px}pre{white-space:pre-wrap;color:#aeb5bf}</style><h1>${escape(summary)}</h1><p>${escape(info.name)}</p><p>Скорость по индикаторам клиентов: Tortuga — оранжевый, uTorrent — зелёный. Сплошная линия — первый проход, пунктир — второй.</p><svg viewBox="0 0 900 250">${chart}</svg><table><tr><th>Клиент</th><th>Проход</th><th>Секунды</th><th>Средняя MiB/s</th><th>Пиры</th><th>CPU %</th><th>Хеши</th></tr>${compact.map(r=>`<tr><td>${r.client}</td><td>${r.round}</td><td>${r.seconds.toFixed(2)}</td><td>${(r.meanPayloadBps/1048576).toFixed(2)}</td><td>${r.meanPeers.toFixed(1)}</td><td>${r.meanCpuPercent.toFixed(2)}</td><td>OK</td></tr>`).join('')}</table><pre>${escape(lines.slice(-8).join('\n'))}</pre></html>`);
}

async function main(){
 const args=process.argv.slice(2),arg=(name,fallback)=>{const i=args.indexOf('--'+name);return i<0?fallback:args[i+1];};
 if(args.includes('--help')){console.log('node benchmark.cjs [--torrent FILE] [--rounds 2] [--timeout 1800] [--output DIR] [--tortuga EXE] [--utorrent EXE] [--keep-downloads] [--cold] [--self-test]');return;}
 const selfTest=args.includes('--self-test');
 let torrent=arg('torrent');if(!torrent&&!selfTest){const downloads=path.join(os.homedir(),'Downloads');const candidates=fs.readdirSync(downloads).filter(n=>/\.torrent$/i.test(n)&&/Invite|Приглашение/i.test(n));if(candidates.length!==1)throw new Error('Expected one Invite .torrent in Downloads; pass --torrent "full path"');torrent=path.join(downloads,candidates[0]);}
 const output=path.resolve(arg('output',path.join(__dirname,'results',new Date().toISOString().replace(/[:.]/g,'-'))));if(fs.existsSync(output))throw new Error('Output directory must be new');fs.mkdirSync(output,{recursive:true});
 const options={output,torrent,rounds:Number(arg('rounds',selfTest?'1':'2')),timeout:Number(arg('timeout','1800')),selfTest,keepDownloads:args.includes('--keep-downloads'),Tortuga:path.resolve(arg('tortuga',path.join(__dirname,'apps','tortuga.exe'))),uTorrent:path.resolve(arg('utorrent',path.join(__dirname,'apps','utorrent.exe')))};
 if(!Number.isInteger(options.rounds)||options.rounds<1||options.rounds>10||!Number.isFinite(options.timeout)||options.timeout<5)throw new Error('Invalid rounds or timeout');
 for(const executable of [options.Tortuga,options.uTorrent])if(!fs.existsSync(executable))throw new Error('Missing executable: '+executable);
 const metrics=new Metrics();let fixture;const rows=[];
 try{
  const environment=await metrics.request({network:true});environment.platform={windows:os.release(),node:process.version,cpu:os.cpus()[0]?.model,logicalCpus:os.cpus().length,memoryBytes:os.totalmem()};const versions=await metrics.request({versions:[options.Tortuga,options.uTorrent]});environment.clients=versions.versions.map(v=>({...v,sha256:crypto.createHash('sha256').update(fs.readFileSync(v.exe)).digest('hex')}));json(path.join(output,'environment.json'),environment);
  if(environment.running.length)throw new Error('Close Tortuga and uTorrent completely before the test (including tray icons).');
  if(environment.vpnActive&&!selfTest)throw new Error('VPN is active. Turn it off manually, then run the test again. The test does not alter VPN settings.');
  options.cache={Tortuga:[],uTorrent:[]};environment.cacheMode=args.includes('--cold')?'cold':'snapshot';environment.cache=[];
  if(!selfTest&&!args.includes('--cold')){const roaming=process.env.APPDATA||path.join(os.homedir(),'AppData','Roaming');for(const [client,source] of [['Tortuga',path.join(roaming,'app.tortuga.client','dht.json')],['uTorrent',path.join(roaming,'utorrent','dht.dat')]])if(fs.existsSync(source)){const folder=path.join(output,'cache',client);fs.mkdirSync(folder,{recursive:true});const snapshot=path.join(folder,path.basename(source));fs.copyFileSync(source,snapshot);options.cache[client].push({source:snapshot,name:path.basename(source)});environment.cache.push({client,name:path.basename(source),sha256:crypto.createHash('sha256').update(fs.readFileSync(snapshot)).digest('hex')});}}
  json(path.join(output,'environment.json'),environment);
  if(selfTest){fixture=await require('./fixture.cjs').start(output,encode,sha);options.torrent=fixture.torrent;}
  const info=torrentInfo(options.torrent);json(path.join(output,'torrent.json'),{source:options.torrent,name:info.name,hash:info.hash,total:info.total});
  for(let round=1;round<=options.rounds;round++)for(const client of round%2===1?['Tortuga','uTorrent']:['uTorrent','Tortuga']){rows.push(await runOne(client,round,options,metrics,info));writeReport(output,rows,info,environment);await sleep(selfTest?500:10000);const network=await metrics.request({network:true});if(network.vpnActive&&!selfTest)throw new Error('VPN appeared during the test; comparison stopped.');}
  const archive=await metrics.request({root:output,archive:path.join(output,'logs-reports.zip')});if(archive.error)throw new Error('Report archive failed: '+archive.error);console.log('\nDone. Reports: '+output+'\nShare logs-reports.zip for analysis.');
 }catch(e){fs.writeFileSync(path.join(output,'failure.txt'),e.stack||String(e));try{await metrics.request({root:output,archive:path.join(output,'logs-reports.zip')});}catch{}throw e;}
 finally{metrics.close();if(fixture)await fixture.close();}
}
module.exports={encode,decode,torrentInfo,verifyFiles,startUtorrent,startTortuga,Metrics,runOne};
if(require.main===module)main().catch(e=>{console.error('\nTEST FAILED: '+e.message);process.exitCode=1;});
