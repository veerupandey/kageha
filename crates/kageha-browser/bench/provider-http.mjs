// Fresh TLS connections avoid poisoning a long-lived evaluation with a stale pool.
// Retrying this transport is safe only for model inference, never browser input.
import https from 'node:https';
export function postJson(url,headers,payload,timeoutMs=45000){
  return new Promise((resolve,reject)=>{
    let settled=false;
    const finish=(error,value)=>{if(settled)return;settled=true;clearTimeout(timer);error?reject(error):resolve(value);};
    const request=https.request(url,{method:'POST',agent:false,headers:{...headers,'Content-Type':'application/json'}},response=>{
      const chunks=[];let length=0;
      response.on('data',chunk=>{length+=chunk.length;if(length>8*1024*1024){request.destroy(Error('provider_response_too_large'));return;}chunks.push(chunk);});
      response.on('error',error=>finish(error));
      response.on('end',()=>{
        const bytes=Buffer.concat(chunks).toString('utf8');
        finish(null,{ok:response.statusCode>=200&&response.statusCode<300,status:response.statusCode,
          headers:{get(name){return response.headers[name.toLowerCase()]??null;}},async json(){return JSON.parse(bytes);}});
      });
    });
    const timer=setTimeout(()=>request.destroy(Error('provider_transport_timeout')),timeoutMs);
    request.on('error',error=>finish(Object.assign(Error(`provider_transport_${error.code??error.message}`),{code:error.code})));
    request.end(JSON.stringify(payload));
  });
}
