/* Copyright 2011-2012 Nicholas J. Kain, licensed under standard MIT license */
/* Nova OS: SYS_MEM_UNMAP then SYS_THREAD_EXIT, on no stack of its own. */
.text
.global __unmapself
.type   __unmapself,@function
__unmapself:
	movl $0x0311,%eax       /* SYS_MEM_UNMAP */
	syscall                 /* munmap(arg2,arg3) */
	xor %rdi,%rdi           /* exit() args: always return success */
	movl $0x0A02,%eax       /* SYS_THREAD_EXIT */
	syscall
