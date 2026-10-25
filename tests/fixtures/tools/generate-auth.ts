// Offline source invocation: every fetch is intercepted before calling auth code.
import { resolve } from 'node:path'
import { writeFileSync } from 'node:fs'
const reference=process.argv[2]
if(!reference)throw new Error('Provide the pinned Agent Messenger checkout')
const login=await import(resolve(reference,'src/platforms/kakaotalk/auth/kakao-login.ts'))
const {refreshKakaoOAuthToken}=await import(resolve(reference,'src/platforms/kakaotalk/auth/oauth-refresh.ts'))
const requests:any[]=[]
const capture=async(url:any,options:any)=>{
 requests.push({url:String(url),headers:options.headers,body:options.body})
 const body=String(url).endsWith('/generate')?{status:0,passcode:'123456',remainingSeconds:30}:String(url).endsWith('/registerDevice')?{status:0}:{status:0,userId:42,access_token:'synthetic-access',refresh_token:'synthetic-refresh',expires_in:3600}
 return new Response(JSON.stringify(body),{status:200,headers:{'Content-Type':'application/json'}})
}
globalThis.fetch=capture as typeof fetch
const email='owner@example.invalid',password='synthetic-password',device='a'.repeat(64)
await login.attemptLogin(email,password,device,'tablet',false)
await login.requestPasscode(email,password,device)
await login.registerDevice(email,password,'',device)
await refreshKakaoOAuthToken({accessToken:'synthetic-access',refreshToken:'synthetic-refresh',deviceUuid:device},{fetchImpl:capture as typeof fetch,requestId:()=> 'source-request-id'})
writeFileSync(resolve(import.meta.dir,'../agent-android-auth-requests.json'),JSON.stringify(requests,null,2)+'\n')
console.log('Captured source HTTP auth/approval/refresh requests with synthetic inputs and intercepted fetch.')
