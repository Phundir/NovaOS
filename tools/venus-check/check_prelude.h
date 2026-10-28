/* C half of the Venus encoder check: the reference encoder. */
#include <inttypes.h>
#include <stdio.h>
#include "vn_protocol_driver.h"

static uint64_t counter = 1;

static uint64_t next_value(void)
{
   return counter++ * 0x9E3779B1u % 1000003u + 1;
}

static const char *next_string(void)
{
   char *s = malloc(32);
   snprintf(s, 32, "s%" PRIu64, next_value());
   return s;
}

static void dump(const char *name, const struct vn_cs_encoder *enc)
{
   printf("%s ", name);
   for (size_t i = 0; i < enc->len; i++)
      printf("%02x", enc->buf[i]);
   printf("\n");
}
