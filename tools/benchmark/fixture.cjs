const fs=require('node:fs');
const path=require('node:path');
const net=require('node:net');
const http=require('node:http');
exports.start=async(root,encode,sha)=>{
 const content=Buffer.alloc(8*1024*1024);for(let i=0;i<content.length;i++)content[i]=(i*73+(i>>>8)*19)&255;
 const pieceSize=128*1024,pieces=[];for(let i=0;i<content.length;i+=pieceSize)pieces.push(sha(content.subarray(i,i+pieceSize)));
 const info={length:content.length,name:'Benchmark-Local-8MiB.bin','piece length':pieceSize,pieces:Buffer.concat(pieces)},infoBytes=encode(info),hash=sha(infoBytes),sockets=new Set();
 const peers=net.createServer(socket=>{
  socket.setNoDelay(true);
  sockets.add(socket);socket.on('close',()=>sockets.delete(socket));socket.on('error',()=>{});
  let buffer=Buffer.alloc(0),handshaken=false;const queue=[];
  const send=body=>{const size=Buffer.alloc(4);size.writeUInt32BE(body.length);socket.write(Buffer.concat([size,body]));};
  const timer=setInterval(()=>{const body=queue.shift();if(body&&!socket.destroyed)send(body);},8);socket.on('close',()=>clearInterval(timer));
  socket.on('data',chunk=>{
   buffer=Buffer.concat([buffer,chunk]);if(!handshaken){if(buffer.length<68)return;if(!buffer.subarray(28,48).equals(hash)){socket.destroy();return;}buffer=buffer.subarray(68);handshaken=true;socket.write(Buffer.concat([Buffer.from('\x13BitTorrent protocol'),Buffer.alloc(8),hash,Buffer.from('-BQ0001-012345678901')]));send(Buffer.concat([Buffer.from([5]),Buffer.alloc(Math.ceil(pieces.length/8),255)]));send(Buffer.from([1]));}
   while(buffer.length>=4){const n=buffer.readUInt32BE(0);if(buffer.length<4+n)return;const msg=buffer.subarray(4,4+n);buffer=buffer.subarray(4+n);if(msg[0]===6){const index=msg.readUInt32BE(1),begin=msg.readUInt32BE(5),size=msg.readUInt32BE(9);queue.push(Buffer.concat([Buffer.from([7]),msg.subarray(1,9),content.subarray(index*pieceSize+begin,index*pieceSize+begin+size)]));}}
  });
 });
 await new Promise(r=>peers.listen(0,'127.0.0.1',r));
 const compact=Buffer.alloc(6);compact.set([127,0,0,1]);compact.writeUInt16BE(peers.address().port,4);
 const tracker=http.createServer((req,res)=>{res.setHeader('Content-Type','application/x-bittorrent');res.end(encode({interval:1,complete:1,incomplete:0,peers:compact}));});await new Promise(r=>tracker.listen(0,'127.0.0.1',r));
 const torrent=path.join(root,'local-fixture.torrent');fs.writeFileSync(torrent,encode({announce:'http://127.0.0.1:'+tracker.address().port+'/announce',info}));
 return{torrent,content,hash:hash.toString('hex'),peer:'127.0.0.1:'+peers.address().port,close:async()=>{for(const s of sockets)s.destroy();await Promise.all([new Promise(r=>peers.close(r)),new Promise(r=>tracker.close(r))]);}};
};
