// Timed input observations while host drives this owned q800 only.
#include <Quickdraw.h>
#include <Fonts.h>
#include <Windows.h>
#include <Menus.h>
#include <TextEdit.h>
#include <Dialogs.h>
#include <Files.h>
#include <cstdio>
#include <cstring>
static void post_mouse_up(){register long d0 asm("d0")=0;register void*a0 asm("a0")=reinterpret_cast<void*>(2);asm volatile(".word 0xa12f":"+d"(d0),"+a"(a0)::"d1","a1","cc","memory");}
int main(){InitGraf(&qd.thePort);InitFonts();InitWindows();InitMenus();TEInit();InitDialogs(nullptr);
 Rect bounds={100,100,350,500};WindowPtr w=NewWindow(nullptr,&bounds,"\pInput reference",true,0,(WindowPtr)-1,true,0);SetPort(w);RGBColor blue={0,0,65535};RGBForeColor(&blue);PaintRect(&w->portRect);ForeColor(blackColor);
 const char*name=":Input Reference.jsonl";unsigned char path[256];path[0]=std::strlen(name);std::memcpy(path+1,name,path[0]);Create(path,0,'ESRP','TEXT');short output;if(FSOpen(path,0,&output)!=noErr)return 1;SetEOF(output,0);
 short old=LMGetSysEvtMask();LMSetSysEvtMask(-1);FlushEvents(-1,0);unsigned long start=TickCount(),last=0;int n=0;bool synthetic=false;
 while(TickCount()-start<1200){unsigned long now=TickCount();if(now-last<3)continue;last=now;unsigned char keys[16];GetKeys(keys);char hex[33];for(int i=0;i<16;++i)std::snprintf(hex+i*2,3,"%02x",keys[i]);Point local;GetMouse(&local);bool button=Button();if(button&&!synthetic){post_mouse_up();synthetic=true;}bool still=StillDown();EventRecord e;bool found=GetOSEvent(-1,&e);char b[700];long len=std::snprintf(b,sizeof(b),"{\"sample\":%d,\"elapsed\":%lu,\"keys\":\"%s\",\"button\":%s,\"still_down\":%s,\"x\":%d,\"y\":%d,\"global_x\":%d,\"global_y\":%d,\"ticks\":%lu,\"lm_ticks\":%lu,\"found\":%s,\"what\":%u,\"message\":%lu,\"event_modifiers\":%u}\r",n++,now-start,hex,button?"true":"false",still?"true":"false",local.h,local.v,LMGetMouseLocation().h,LMGetMouseLocation().v,now,(unsigned long)LMGetTicks(),found?"true":"false",(unsigned)e.what,(unsigned long)e.message,(unsigned)e.modifiers);FSWrite(output,&len,b);}
 const char*done="{\"complete\":true}\r";long size=std::strlen(done);FSWrite(output,&size,done);FSClose(output);FlushVol(nullptr,0);LMSetSysEvtMask(old);DisposeWindow(w);return 0;}
