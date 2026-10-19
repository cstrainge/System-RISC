#include <Quickdraw.h>
#include <Fonts.h>
#include <Windows.h>
#include <Menus.h>
#include <TextEdit.h>
#include <Dialogs.h>
#include <Files.h>
#include <cstdio>
#include <cstring>
static short output;
static void post(short kind){register long d0 asm("d0")=0;register void*a0 asm("a0")=reinterpret_cast<void*>(static_cast<long>(kind));asm volatile(".word 0xa12f":"+d"(d0),"+a"(a0)::"d1","a1","cc","memory");}
static void state(const char*name){char b[300];bool button=Button(),still=StillDown();int count=0;for(QElemPtr q=LMGetEventQueue().qHead;q;q=q->evQElem.qLink)++count;long n=std::snprintf(b,sizeof(b),"{\"case\":\"%s\",\"button\":%s,\"still_down\":%s,\"queue_count\":%d}\r",name,button?"true":"false",still?"true":"false",count);FSWrite(output,&n,b);}
int main(){InitGraf(&qd.thePort);InitFonts();InitWindows();InitMenus();TEInit();InitDialogs(nullptr);const char*name=":Mouse Queue Reference.jsonl";unsigned char path[256];path[0]=std::strlen(name);std::memcpy(path+1,name,path[0]);Create(path,0,'ESRP','TEXT');if(FSOpen(path,0,&output)!=noErr)return 1;SetEOF(output,0);short old=LMGetSysEvtMask();LMSetSysEvtMask(-1);FlushEvents(-1,0);state("idle_empty");post(1);state("idle_synthetic_down");post(2);state("idle_down_then_up");EventRecord e;GetOSEvent(-1,&e);state("idle_synthetic_up");GetOSEvent(-1,&e);state("idle_empty_after");LMSetSysEvtMask(old);const char*done="{\"complete\":true}\r";long n=std::strlen(done);FSWrite(output,&n,done);FSClose(output);FlushVol(nullptr,0);return 0;}
