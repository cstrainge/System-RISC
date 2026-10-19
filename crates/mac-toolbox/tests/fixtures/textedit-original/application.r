#include "Processes.r"
#include "Types.r"
resource 'SIZE' (-1) {
reserved, ignoreSuspendResumeEvents, reserved, cannotBackground,
needsActivateOnFGSwitch, backgroundAndForeground, dontGetFrontClicks,
ignoreChildDiedEvents, is32BitCompatible, isHighLevelEventAware,
onlyLocalHLEvents, notStationeryAware, dontUseTextEditServices,
reserved, reserved, reserved, 4*1024*1024, 2*1024*1024
};
