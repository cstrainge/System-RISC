// Isolated execution on original System7.6.1; observations only.
#include <Quickdraw.h>
#include <Fonts.h>
#include <Windows.h>
#include <Menus.h>
#include <TextEdit.h>
#include <Dialogs.h>
#include <Files.h>
#include <cstdio>
#include <cstring>
#include <initializer_list>
static short output; static int sequence;
static void line(const char*s){long n=std::strlen(s);FSWrite(output,&n,s);}
static void scrap(const char*action,long status,long offset=-777,Handle h=nullptr){
 ScrapStuff*s=InfoScrap();char hex[1025];int n=0;long size=h?GetHandleSize(h):0;
 if(h&&*h)for(long i=0;i<size&&i<128;++i){std::snprintf(hex+n,3,"%02x",(unsigned char)(*h)[i]);n+=2;}
 hex[n]=0;
 char raw[1025];n=0;if(s->scrapHandle&&*s->scrapHandle&&s->scrapState>0)for(long i=0;i<s->scrapSize&&i<128;++i){std::snprintf(raw+n,3,"%02x",(unsigned char)(*s->scrapHandle)[i]);n+=2;}raw[n]=0;
 char info[33];for(int i=0;i<16;++i)std::snprintf(info+i*2,3,"%02x",reinterpret_cast<unsigned char*>(s)[i]);
 char b[3000];std::snprintf(b,sizeof(b),"{\"sequence\":%d,\"group\":\"scrap\",\"action\":\"%s\",\"status\":%ld,\"offset\":%ld,\"size\":%ld,\"count\":%d,\"state\":%d,\"handle_size\":%ld,\"bytes\":\"%s\",\"raw\":\"%s\",\"info_raw\":\"%s\"}\r",sequence++,action,status,offset,s->scrapSize,s->scrapCount,s->scrapState,size,hex,raw,info);line(b);
}
static void get(const char*action,ResType type,bool null_handle=false){Handle h=null_handle?nullptr:NewHandleClear(7);long offset=-777;long result=GetScrap(h,type,&offset);scrap(action,result,offset,h);if(h)DisposeHandle(h);}
static void scraps(){
 const char abc[]="abc";const char even[]="1234";const unsigned char text[]={0x41,0x80,0x8e,0x0d,0};
 scrap("initial",0);scrap("zero_first",ZeroScrap());scrap("zero_second",ZeroScrap());get("missing_empty",'TEXT');
 scrap("put_odd_TEXT",PutScrap(3,'TEXT',const_cast<char*>(abc)));get("get_TEXT",'TEXT');get("query_TEXT_nil",'TEXT',true);
 scrap("put_even_PICT",PutScrap(4,'PICT',const_cast<char*>(even)));get("get_PICT",'PICT');get("missing_flavor",'XXXX');
 scrap("put_repeat_TEXT",PutScrap(5,'TEXT',reinterpret_cast<Ptr>(const_cast<unsigned char*>(text))));get("repeat_get_TEXT",'TEXT');get("repeat_get_PICT",'PICT');
 scrap("put_zero_TEST",PutScrap(0,'TEST',const_cast<char*>(abc)));get("get_zero_TEST",'TEST');
 scrap("put_zero_TEXT",PutScrap(0,'TEXT',const_cast<char*>(abc)));get("get_zero_TEXT",'TEXT');
 scrap("zero_final",ZeroScrap());get("get_after_zero",'TEXT');
}
static int qcount(){int n=0;for(QElemPtr p=LMGetEventQueue().qHead;p&&n<256;p=p->evQElem.qLink)++n;return n;}
static void event(const char*action,bool found,const EventRecord&e,int status=0,bool element=false){char b[700];std::snprintf(b,sizeof(b),"{\"sequence\":%d,\"group\":\"events\",\"action\":\"%s\",\"found\":%s,\"status\":%d,\"element\":%s,\"what\":%u,\"message\":%lu,\"when\":%lu,\"x\":%d,\"y\":%d,\"modifiers\":%u,\"queue_count\":%d}\r",sequence++,action,found?"true":"false",status,element?"true":"false",(unsigned)e.what,(unsigned long)e.message,(unsigned long)e.when,e.where.h,e.where.v,(unsigned)e.modifiers,qcount());line(b);}
static void read(const char*action,short mask,bool peek=false){EventRecord e;std::memset(&e,0xa5,sizeof(e));bool found=peek?OSEventAvail(mask,&e):GetOSEvent(mask,&e);event(action,found,e);}
static short original_post(short kind,long message,EvQElPtr* result){register long d0 asm("d0")=message;register EvQElPtr a0 asm("a0")=reinterpret_cast<EvQElPtr>(static_cast<unsigned long>(kind));asm volatile(".word 0xa12f":"+d"(d0),"+a"(a0)::"d1","a1","cc","memory");*result=a0;return static_cast<short>(d0);}
static void post(const char*action,short kind,long message){EvQElPtr p=nullptr;short status=original_post(kind,message,&p);EventRecord e={};bool linked=false;for(QElemPtr q=LMGetEventQueue().qHead;q;q=q->evQElem.qLink)if(reinterpret_cast<EvQElPtr>(q)==p)linked=true;if(linked){e.what=p->evtQWhat;e.message=p->evtQMessage;e.when=p->evtQWhen;e.where=p->evtQWhere;e.modifiers=p->evtQModifiers;}event(action,false,e,status,linked);}
static void events(){
 FlushEvents(-1,0);short oldmask=LMGetSysEvtMask();LMSetSysEvtMask(-1);read("empty_null",-1);
 post("post_key3_A",3,0x111);post("post_mouse1_B",1,0x222);post("post_key3_C",3,0x333);
 read("peek_mouse1",2,true);read("peek_mouse1_again",2,true);read("get_mouse1",2);read("get_all_first",-1);read("get_all_second",-1);read("get_all_empty",-1);
 post("flush_key_A",3,0x444);post("flush_mouse_B",1,0x555);post("flush_key_C",3,0x666);FlushEvents(8,2);read("flush_stop_first",-1);read("flush_stop_second",-1);read("flush_stop_empty",-1);
 post("overlap_key_A",3,0x777);post("overlap_mouse_B",1,0x888);post("overlap_key_C",3,0x999);FlushEvents(-1,2);read("overlap_stop_first",-1);read("overlap_stop_second",-1);read("overlap_stop_empty",-1);
 LMSetSysEvtMask(2);post("mask_disabled_key",3,0xabc);post("mask_enabled_mouse",1,0xdef);read("masked_read",-1);LMSetSysEvtMask(-1);FlushEvents(-1,0);
 for(short kind:{0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,23,31}){char a[40];std::snprintf(a,sizeof(a),"kind_%d_post",kind);post(a,kind,0x1000+kind);std::snprintf(a,sizeof(a),"kind_%d_read",kind);read(a,-1);FlushEvents(-1,0);}
 FlushEvents(-1,0);for(int i=0;i<40;++i){char a[40];std::snprintf(a,sizeof(a),"capacity_post_%d",i);post(a,3,i+1);}for(int i=0;i<24;++i){char a[40];std::snprintf(a,sizeof(a),"capacity_get_%d",i);read(a,-1);}FlushEvents(-1,0);
 unsigned char keys[16];GetKeys(keys);char hex[33];for(int i=0;i<16;++i)std::snprintf(hex+i*2,3,"%02x",keys[i]);Point local;GetMouse(&local);char b[512];std::snprintf(b,sizeof(b),"{\"sequence\":%d,\"group\":\"input\",\"action\":\"idle\",\"keys\":\"%s\",\"button\":%s,\"still_down\":%s,\"x\":%d,\"y\":%d,\"global_x\":%d,\"global_y\":%d,\"ticks\":%lu,\"lm_ticks\":%lu}\r",sequence++,hex,Button()?"true":"false",StillDown()?"true":"false",local.h,local.v,LMGetMouseLocation().h,LMGetMouseLocation().v,(unsigned long)TickCount(),(unsigned long)LMGetTicks());line(b);LMSetSysEvtMask(oldmask);
}
int main(){InitGraf(&qd.thePort);InitFonts();InitWindows();InitMenus();TEInit();InitDialogs(nullptr);const char*name=":Events Scrap Reference.jsonl";unsigned char path[256];path[0]=std::strlen(name);std::memcpy(path+1,name,path[0]);Create(path,0,'ESRP','TEXT');if(FSOpen(path,0,&output)!=noErr)return 1;SetEOF(output,0);scraps();events();line("{\"complete\":true}\r");FSClose(output);FlushVol(nullptr,0);return 0;}
