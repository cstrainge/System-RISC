// Bounded observations from the original mono TextEdit implementation.
#include <Quickdraw.h>
#include <Fonts.h>
#include <Windows.h>
#include <Menus.h>
#include <TextEdit.h>
#include <Dialogs.h>
#include <Files.h>
#include <cstdio>
#include <cstring>
#include <cstddef>
#include <cstdarg>
#include <initializer_list>

static short output;
static void write(const char* format, ...){char line[8192];va_list args;va_start(args,format);long n=std::vsnprintf(line,sizeof(line),format,args);va_end(args);if(n>0&&n<(long)sizeof(line))FSWrite(output,&n,line);}
static void hex(const char* p,long n){for(long i=0;i<n;++i)write("%02x",(unsigned char)p[i]);}
static void rect(const Rect&r){write("[%d,%d,%d,%d]",r.left,r.top,r.right,r.bottom);}
static void emit(const char* test,const char* operation,TEHandle te){
 HLock((Handle)te);TEPtr p=*te;write("{\"case\":\"%s\",\"operation\":\"%s\",\"text_hex\":\"",test,operation);hex(*p->hText,p->teLength);
 write("\",\"length\":%d,\"selection\":[%d,%d],\"n_lines\":%d,\"line_height\":%d,\"font_ascent\":%d,\"font\":%d,\"size\":%d,\"face\":%d,\"dest\":",p->teLength,p->selStart,p->selEnd,p->nLines,p->lineHeight,p->fontAscent,p->txFont,p->txSize,p->txFace);rect(p->destRect);write(",\"view\":");rect(p->viewRect);write(",\"line_starts\":[");
 long capacity=(GetHandleSize((Handle)te)-offsetof(TERec,lineStarts))/2;for(short i=0;i<=p->nLines&&i<capacity;++i)write("%s%d",i?",":"",p->lineStarts[i]);
 write("],\"points\":[");for(short i=0;i<=p->teLength;++i){long pt=TEGetPoint(i,te);write("%s[%d,%d]",i?",":"",(short)(pt&65535),(short)(pt>>16));}write("]}\r");HUnlock((Handle)te);
}
static TEHandle make(const char* text,short width=100){Rect dest={4,6,90,(short)(6+width)},view={8,10,80,120};TEHandle te=TENew(&dest,&view);TESetText(const_cast<char*>(text),std::strlen(text),te);return te;}
static void layout(){
 struct Case{const char*name;const char*text;short width;};
 const Case cases[]={{"empty","",100},{"single","abc",100},{"cr","abc\rdef",100},{"trailing_cr","abc\r",100},{"crlf","abc\r\ndef",100},{"tab","a\tb",100},{"wrap","one two three four five six",60},{"narrow","abc def",5},{"spaces","  one two   ",45},{"trailing_spaces","abc     def",30},{"exact","MMMM",44},{"double_cr","a\r\rb",100}};
 for(auto c:cases){TEHandle te=make(c.text,c.width);emit(c.name,"set_text",te);TECalText(te);emit(c.name,"cal_text",te);TEDispose(te);}
}
static void selection(){TEHandle te=make("abc\rdef");const long ranges[][2]={{0,0},{1,5},{5,1},{-1,4},{2,32767},{32767,32767},{-10,-1},{0,65535}};for(auto&r:ranges){char op[64];std::snprintf(op,sizeof(op),"select_%ld_%ld",r[0],r[1]);TESetSelect(r[0],r[1],te);emit("selection",op,te);}TEDispose(te);}
static void alignment(){for(short align:{-1,0,1}){TEHandle te=make("abc\rdef ghij",60);TESetAlignment(align,te);char name[30];std::snprintf(name,sizeof(name),"alignment_%d",align);emit(name,"align",te);TEDispose(te);}}
static void editing(){
 struct Edit{const char*name;short start,end;char key;};
 const Edit edits[]={{"backspace_start",0,0,8},{"backspace_middle",3,3,8},{"backspace_selection",1,5,8},{"delete_forward",3,3,127},{"left_start",0,0,28},{"left_middle",3,3,28},{"left_selection",1,5,28},{"right_end",7,7,29},{"right_selection",1,5,29},{"up",5,5,30},{"down",1,1,31},{"return",2,2,13},{"tab",2,2,9},{"printable_selection",1,5,'X'}};
 for(auto c:edits){TEHandle te=make("abc\rdef");TESetSelect(c.start,c.end,te);TEKey(c.key,te);emit(c.name,"key",te);TEDispose(te);}
 for(short selected:{0,1}){TEHandle te=make("abcdef");TESetSelect(2,selected?4:2,te);TEInsert(const_cast<char*>("XY"),2,te);emit(selected?"insert_selection":"insert_caret","insert",te);TEDispose(te);te=make("abcdef");TESetSelect(2,selected?4:2,te);TEDelete(te);emit(selected?"delete_selection":"delete_caret","delete",te);TEDispose(te);}
}
static void offsets(){TEHandle te=make("abc\rdef ghij",40);for(short y:{-10,3,4,15,16,19,20,31,32,47,80}){write("{\"case\":\"offsets\",\"y\":%d,\"offsets\":[",y);short i=0;for(short x:{-10,5,6,7,8,9,10,11,12,13,14,15,16,20,25,30,40,60,130}){Point p={y,x};write("%s[%d,%d]",i++?",":"",x,TEGetOffset(p,te));}write("]}\r");}TEDispose(te);}
static void scrap_emit(const char* op){write("{\"case\":\"scrap\",\"operation\":\"%s\",\"te_length\":%ld,\"te_hex\":\"",op,(long)LMGetTEScrpLength());Handle h=LMGetTEScrpHandle();hex(h&&*h?*h:"",(long)LMGetTEScrpLength());Handle copy=NewHandle(0);long offset=0;long n=GetScrap(copy,'TEXT',&offset);write("\",\"system_length\":%ld,\"system_hex\":\"",n);if(n>0)hex(*copy,n);write("\"}\r");DisposeHandle(copy);}
static void scraps(){ZeroScrap();LMSetTEScrpLength(0);TEHandle te=make("abcdef");TESetSelect(1,4,te);TECopy(te);emit("copy","copy",te);scrap_emit("copy");ZeroScrap();PutScrap(3,'TEXT',const_cast<char*>("XYZ"));scrap_emit("system_replaced");TEPaste(te);emit("paste_private","paste",te);TESetSelect(0,2,te);TECut(te);emit("cut","cut",te);scrap_emit("cut");TESetSelect(1,1,te);TECopy(te);scrap_emit("copy_caret");TESetSelect(0,2,te);TEPaste(te);emit("paste_empty_private","paste",te);TEDispose(te);}
static void pixels(){Rect bounds={0,0,96,128};GWorldPtr world=nullptr;NewGWorld(&world,1,&bounds,nullptr,nullptr,0);if(!world)return;SetGWorld(world,nullptr);LockPixels(GetGWorldPixMap(world));TextFont(0);TextSize(12);TextFace(0);TextMode(srcOr);
 const char* names[]={"text","selection","caret","scroll","caret_idle_on","caret_idle_off","inactive_selection"};
 for(short n=0;n<7;++n){EraseRect(&bounds);TEHandle te=make("one two three\rfour five six\rseven eight",80);TEUpdate(&bounds,te);if(n==1||n==6){TESetSelect(2,22,te);TEActivate(te);if(n==6)TEDeactivate(te);}if(n==2||n==4||n==5){TESetSelect(5,5,te);TEActivate(te);if(n==4||n==5){(**te).caretTime=TickCount()-LMGetCaretTime()-1;TEIdle(te);}if(n==5){(**te).caretTime=TickCount()-LMGetCaretTime()-1;TEIdle(te);}}if(n==3)TEScroll(-5,-16,te);
 write("{\"case\":\"visual_state\",\"operation\":\"%s\",\"active\":%d,\"caret_state\":%d,\"selection_rect\":",names[n],(**te).active,(**te).caretState);rect((**te).selRect);write("}\r");
 PixMapHandle pm=GetGWorldPixMap(world);short stride=(**pm).rowBytes&0x3fff;char*base=GetPixBaseAddr(pm);write("{\"case\":\"pixels\",\"operation\":\"%s\",\"width\":128,\"height\":96,\"rows\":[",names[n]);for(short y=0;y<96;++y){write("%s\"",y?",":"");hex(base+y*stride,16);write("\"");}write("]}\r");TEDeactivate(te);TEDispose(te);}
 UnlockPixels(GetGWorldPixMap(world));SetGWorld((CGrafPtr)qd.thePort,nullptr);DisposeGWorld(world);
}
int main(){InitGraf(&qd.thePort);InitFonts();InitWindows();InitMenus();TEInit();InitDialogs(nullptr);TextFont(0);TextSize(12);TextFace(0);TextMode(srcOr);const char*name=":TextEdit.jsonl";unsigned char path[256];path[0]=std::strlen(name);std::memcpy(path+1,name,path[0]);Create(path,0,'TERP','TEXT');if(FSOpen(path,0,&output)!=noErr)return 1;SetEOF(output,0);layout();selection();alignment();editing();offsets();scraps();pixels();write("{\"complete\":true}\r");FSClose(output);FlushVol(nullptr,0);return 0;}
