// Offline harness only: intercept every network method before invoking the source.
import { writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
const reference = process.argv[2]
if (!reference) throw new Error('Provide the exact Agent Messenger checkout path')
const { LocoSession } = await import(resolve(reference, 'src/platforms/kakaotalk/protocol/session.ts'))
const { LocoConnection } = await import(resolve(reference, 'src/platforms/kakaotalk/protocol/connection.ts'))
const { encodePacket } = await import(resolve(reference, 'src/platforms/kakaotalk/protocol/packet.ts'))
const base=resolve(import.meta.dir,'..')+'/'
LocoConnection.prototype.connectTls=async()=>{}
LocoConnection.prototype.connectSecure=async()=>{}
LocoConnection.prototype.close=()=>{}
LocoConnection.prototype.sendPacket=async function(method,body={}) {
  writeFileSync(base+'agent-'+method.toLowerCase()+'.bin',encodePacket({packetId:1,statusCode:0,method,bodyType:0,body}))
  const response=method==='GETCONF'?{hosts:['checkin.example'],wifi:{ports:[995]}}:method==='CHECKIN'?{host:'chat.example',port:5223}:{status:0,userId:42,chatDatas:[],eof:true,revision:0}
  writeFileSync(base+'agent-'+method.toLowerCase()+'-response.bin',encodePacket({packetId:1,statusCode:0,method,bodyType:0,body:response}))
  return {packetId:1,statusCode:0,method,bodyType:0,body:response}
}
const session=new LocoSession()
try { await session.login('synthetic-token','42','synthetic-device',undefined,'pc') } finally {session.close()}
console.log('Captured GETCONF/CHECKIN/LOGINLIST from source with all sockets replaced, synthetic fields only.')
