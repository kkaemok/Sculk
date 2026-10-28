// Dev-only source contract generator: npm install esbuild bson@6 zod@4 in a
// temporary prefix and set NODE_PATH to that prefix's node_modules. No Rust
// runtime dependency. All networking imports are replaced before execution.
const {build}=require('esbuild');
const {resolve}=require('node:path');
const {execFileSync}=require('node:child_process');
const {writeFileSync}=require('node:fs');
const {runInNewContext}=require('node:vm');
(async()=>{
  const reference=process.argv[2];
  if(!reference)throw Error('Provide the pinned Agent Messenger checkout');
  const sha=execFileSync('git',['-C',reference,'rev-parse','HEAD'],{encoding:'utf8'}).trim();
  if(sha!=='9d2de96a43fe3e0e172e8e399454c475146a2697')throw Error('Unexpected source snapshot');
  const base=resolve(reference,'src/platforms/kakaotalk/protocol');
  const result=await build({stdin:{contents:`
    import {Long} from 'bson';
    import {LocoSession} from ${JSON.stringify(resolve(base,'session.ts'))};
    import {encodePacket} from ${JSON.stringify(resolve(base,'packet.ts'))};
    const session=new LocoSession();
    // Only replace the connection boundary; execute the original sendMessage.
    session.connection={sendPacket:async(method,body)=>{
      const packet={packetId:1,statusCode:0,method,bodyType:0,body};
      capture("write",encodePacket(packet));return packet;
    }};
    session.sendMessage(Long.fromString('9007199254740993'),'Synthetic text').catch(e=>{throw e});
    // Synthetic MsgRes/ChatlogStruct fields from the pinned node-kakao schema;
    // only framing is produced by executable source. This is NOT a live capture.
    capture('msg',encodePacket({packetId:0,statusCode:0,method:'MSG',bodyType:0,body:{
      chatId:Long.fromString('9007199254740993'),logId:Long.fromString('9007199254740994'),noSeen:true,
      chatLog:{chatId:Long.fromString('9007199254740993'),logId:Long.fromString('9007199254740994'),
      authorId:Long.fromString('9007199254740995'),type:1,message:'Synthetic text',
      sendAt:1700000000,attachment:'{}',msgId:1,prevId:Long.ZERO}
    }}));
  `,resolveDir:base},bundle:true,write:false,platform:'node',format:'cjs',
    nodePaths:(process.env.NODE_PATH||'').split(require('node:path').delimiter).filter(Boolean),
    plugins:[{name:'no-network',setup(build){
      build.onResolve({filter:/^\.\/connection$/},()=>({path:'mock',namespace:'no-network'}));
      build.onLoad({filter:/.*/,namespace:'no-network'},()=>({contents:'export class LocoConnection { constructor(){throw Error("network construction forbidden")} }',loader:'js'}));
    }}]});
  const captured={};
  // node:crypto/buffer imports supply BSON randomness/primitives only; actual
  // net/tls connection module has been removed from the bundled dependency graph.
  runInNewContext(result.outputFiles[0].text,{require,Buffer,TextEncoder,TextDecoder,process:{platform:process.platform},
    setInterval(){throw Error('timer forbidden')},clearInterval(){},
    capture(name,bytes){if(captured[name])throw Error('duplicate capture');captured[name]=bytes;}});
  if(!captured.write||!captured.msg)throw Error('Missing source contract');
  writeFileSync(resolve(__dirname,'../agent-write-request.bin'),captured.write);
  writeFileSync(resolve(__dirname,'../node-msg-synthetic.bin'),captured.msg);
  console.log('Generated one offline WRITE from actual source sendMessage; no sockets or authentication.');
})().catch(e=>{console.error(e);process.exitCode=1});
