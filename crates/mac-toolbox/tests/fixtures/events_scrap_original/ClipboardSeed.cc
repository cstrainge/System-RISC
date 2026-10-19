#include <Quickdraw.h>
#include <Fonts.h>
#include <Windows.h>
#include <Menus.h>
#include <TextEdit.h>
#include <Dialogs.h>
int main(){InitGraf(&qd.thePort);InitFonts();InitWindows();InitMenus();TEInit();InitDialogs(nullptr);ZeroScrap();
#ifndef EMPTY
 const unsigned char text[]={'a','b','c'};PutScrap(sizeof(text),'TEXT',reinterpret_cast<Ptr>(const_cast<unsigned char*>(text)));
#endif
 return 0;}
