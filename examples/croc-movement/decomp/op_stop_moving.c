#include "names.h"

/* Opcode A0 (0x8004317C): Croc stops dead. Steps the script past the
   opcode, then clears the smoothed speed, the gait, the walk counter and
   the turn rate. */
void op_stop_moving(Object *obj)
{
    obj->pc += 1;
    speed_z = 0;
    speed_x = 0;
    speed = 0;
    move_state = 0;
    walk_counter = 0;
    turn_rate = 0;
}
