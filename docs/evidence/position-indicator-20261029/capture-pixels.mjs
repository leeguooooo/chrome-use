// Analyze the fixture marker pixels, allowing for the screenshot color profile.
import {readFileSync,writeFileSync,existsSync} from 'node:fs';import{inflateSync}from'node:zlib';import{join}from'node:path';
const out=process.argv[2];const summary={};
const captures=JSON.parse(readFileSync(join(out,'capture-matrix.json'),'utf8')).captures;
for(const {label,error} of captures){
 if(!/^[a-z0-9-]+$/.test(label))throw Error('Invalid capture label');
 if(error)continue;
 const file=join(out,label+'.png');if(!existsSync(file))throw Error('Missing expected PNG: '+label);
 const png=readFileSync(file);let offset=8,width,height,channels,idat=[];
 while(offset<png.length){const size=png.readUInt32BE(offset),type=png.toString('ascii',offset+4,offset+8),data=png.subarray(offset+8,offset+8+size);offset+=size+12;if(type==='IHDR'){width=data.readUInt32BE(0);height=data.readUInt32BE(4);if(data[8]!==8||![2,6].includes(data[9])||data[12]!==0)throw Error('Unsupported PNG');channels=data[9]===2?3:4}if(type==='IDAT')idat.push(data)}
 const raw=inflateSync(Buffer.concat(idat));let cursor=0,previous=Buffer.alloc(width*channels),count=0,bounds=[width,height,0,0];
 const paeth=(a,b,c)=>{const p=a+b-c,pa=Math.abs(p-a),pb=Math.abs(p-b),pc=Math.abs(p-c);return pa<=pb&&pa<=pc?a:pb<=pc?b:c};
 for(let y=0;y<height;y++){const filter=raw[cursor++],row=Buffer.from(raw.subarray(cursor,cursor+width*channels));cursor+=row.length;
 for(let x=0;x<row.length;x++){const a=x>=channels?row[x-channels]:0,b=previous[x],c=x>=channels?previous[x-channels]:0;const predictor=filter===0?0:filter===1?a:filter===2?b:filter===3?Math.floor((a+b)/2):filter===4?paeth(a,b,c):NaN;if(!Number.isFinite(predictor))throw Error('Bad filter');row[x]=(row[x]+predictor)&255}
 for(let x=0;x<width;x++){const i=x*channels;if(row[i]>200&&row[i+1]<100&&row[i+2]>200){count++;bounds=[Math.min(bounds[0],x),Math.min(bounds[1],y),Math.max(bounds[2],x),Math.max(bounds[3],y)]}}previous=row}
 summary[label]={width,height,magentaPixels:count,bounds:count?bounds:null};
}
if(!Object.keys(summary).length)throw Error('No captures analyzed');
writeFileSync(join(out,'pixels.json'),JSON.stringify(summary,null,2));console.log(JSON.stringify(summary));
