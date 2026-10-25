// Source-generated read-only request contracts, synthetic responses; zero real sockets.
import {resolve} from 'node:path'
import {writeFileSync} from 'node:fs'
const reference=process.argv[2]
if(!reference)throw new Error('Provide the pinned Agent Messenger checkout')
const {Long}=await import(resolve(reference,'node_modules/bson'))
const {LocoSession}=await import(resolve(reference,'src/platforms/kakaotalk/protocol/session.ts'))
const {LocoConnection}=await import(resolve(reference,'src/platforms/kakaotalk/protocol/connection.ts'))
const {encodePacket}=await import(resolve(reference,'src/platforms/kakaotalk/protocol/packet.ts'))
const id=Long.fromString('9007199254740993')
const replies:any={GETCONF:{hosts:['checkin.example'],wifi:{ports:[995]}},CHECKIN:{host:'chat.example',port:5223},LOGINLIST:{status:0,userId:42,chatDatas:[],eof:true},LCHATLIST:{chatDatas:[{c:id,t:'OM',a:2}],eof:false,lastTokenId:Long.fromNumber(1),lastChatId:id},CHATINFO:{chatInfo:{chatId:id,type:'OM',activeMembersCount:2,chatMetas:[{type:1,content:'{"title":"Synthetic"}'}]}},GETMEM:{members:[{userId:id,nickName:'Synthetic member'}],token:1}}
LocoConnection.prototype.connectTls=async()=>{}
LocoConnection.prototype.connectSecure=async()=>{}
LocoConnection.prototype.close=()=>{}
LocoConnection.prototype.sendPacket=async function(method,body={}){
 const packet={packetId:1,statusCode:0,method,bodyType:0,body}
 if(['LCHATLIST','CHATINFO','GETMEM'].includes(method)){
  writeFileSync(resolve(import.meta.dir,`../agent-${method.toLowerCase()}-request.bin`),encodePacket(packet))
  writeFileSync(resolve(import.meta.dir,`../agent-${method.toLowerCase()}-response.bin`),encodePacket({...packet,body:replies[method]}))
 }
 return {...packet,body:replies[method]}
}
const session=new LocoSession()
try{await session.login('synthetic-token','42','synthetic-device',undefined,'pc');await session.getChatList();await session.getChannelInfo(id);await session.getAllMembers(id)}finally{session.close()}
console.log('Generated offline read-only contracts from actual source methods; synthetic server data only.')
