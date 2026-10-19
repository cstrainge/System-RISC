#include <Quickdraw.h>
#include <Fonts.h>
#include <Windows.h>
#include <Menus.h>
#include <TextEdit.h>
#include <Dialogs.h>
#include <Events.h>
#include <OSUtils.h>
#ifdef PROBE_ALPHA
#define TITLE "\013Alpha Probe"
#define TOP 80
#else
#define TITLE "\012Zulu Probe"
#define TOP 160
#endif
int main() {
 InitGraf(&qd.thePort); InitFonts(); InitWindows(); InitMenus();
 TEInit(); InitDialogs(nullptr); InitCursor();
 auto apple=NewMenu(128,(const unsigned char*)"\001\024");
 AppendMenu(apple,(const unsigned char*)"\005About");InsertMenu(apple,0);
 auto file=NewMenu(129,(const unsigned char*)"\004File");
 AppendMenu(file,(const unsigned char*)"\006Quit/Q");InsertMenu(file,0);DrawMenuBar();
 Rect bounds={TOP,80,TOP+110,380};
 auto w=NewWindow(nullptr,&bounds,(const unsigned char*)TITLE,true,documentProc,(WindowPtr)-1,true,0);
 bool done=false; EventRecord e;
 while(!done) {
  if(!WaitNextEvent(everyEvent,&e,20,nullptr)) continue;
  if(e.what==mouseDown) {
   WindowPtr target=nullptr; auto part=FindWindow(e.where,&target);
   if(part==inMenuBar) {auto m=MenuSelect(e.where);if(HiWord(m)==129&&LoWord(m)==1)done=true;HiliteMenu(0);}
   else if(part==inDrag) {Rect limit=qd.screenBits.bounds;DragWindow(target,e.where,&limit);}
   else if(part==inGoAway&&TrackGoAway(target,e.where))done=true;
   else if(target)SelectWindow(target);
  } else if(e.what==keyDown&&e.modifiers&cmdKey) {
   auto m=MenuKey(e.message&charCodeMask);if(HiWord(m)==129&&LoWord(m)==1)done=true;HiliteMenu(0);
  } else if(e.what==updateEvt) {
   BeginUpdate((WindowPtr)e.message);SetPort((WindowPtr)e.message);
   MoveTo(12,28);DrawString((const unsigned char*)TITLE);EndUpdate((WindowPtr)e.message);
  }
 }
 DisposeWindow(w);return 0;
}
