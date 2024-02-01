use core::{fmt, marker::PhantomData, ptr, usize};

use crate::Arena;

/// A mutable reference to something that has been allocated inside an Arena.
///
/// Importantly, it's stored as a byte offset into the arena's memory,
/// which means it can be serialized to/from disk and still work.
///
/// This also means that dereferencing it requires passing in the arena
/// where it was originally allocated. In debug builds, dereferencing will
/// do a check to make sure the arena being passed in is the same one that
/// was originally used to allocate the reference. (If not, it will panic.)
/// In release builds, this information is stored and nothing is checked at runtime.
pub struct ArenaRefMut<'a, T> {
    byte_offset_into_arena: u32,
    _marker: PhantomData<&'a T>,

    #[cfg(debug_assertions)]
    pub(crate) arena: &'a Arena<'a>,
}

impl<'a, T> Eq for ArenaRefMut<'a, T> {}

impl<'a, T> PartialEq for ArenaRefMut<'a, T> {
    fn eq(&self, other: &Self) -> bool {
        self.byte_offset_into_arena == other.byte_offset_into_arena
    }
}

impl<'a, T: Clone> Clone for ArenaRefMut<'a, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<'a, T: Copy> Copy for ArenaRefMut<'a, T> {}

impl<'a, T: fmt::Debug> fmt::Debug for ArenaRefMut<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let arg;

        #[cfg(debug_assertions)]
        {
            arg = self.as_ref(self.arena).fmt(f);
        }

        #[cfg(not(debug_assertions))]
        {
            arg = self.byte_offset_into_arena;
        }

        write!(f, "ArenaRefMut({:?})", arg)
    }
}

impl<'a, T> ArenaRefMut<'a, T> {
    pub(crate) const fn new_in(byte_offset_into_arena: u32, _arena: &Arena<'a>) -> Self {
        Self {
            byte_offset_into_arena,
            _marker: PhantomData,
            #[cfg(debug_assertions)]
            arena: _arena,
        }
    }

    pub(crate) const fn byte_offset(self) -> usize {
        self.byte_offset_into_arena as usize
    }

    pub(crate) const fn add_bytes(self, amount: u32) -> Self {
        Self {
            byte_offset_into_arena: self.byte_offset_into_arena + amount,
            _marker: PhantomData,

            #[cfg(debug_assertions)]
            arena: self.arena,
        }
    }

    pub fn as_ref(&'a self, arena: &Arena<'a>) -> &'a T {
        #[cfg(debug_assertions)]
        {
            self.debug_verify_arena(arena, "ArenaRefMut::deref");
        }

        unsafe { &*arena.chunk.add(self.byte_offset()).cast() }
    }

    pub fn as_mut(&'a mut self, arena: &Arena<'a>) -> &'a mut T {
        #[cfg(debug_assertions)]
        {
            self.debug_verify_arena(arena, "ArenaRefMut::deref");
        }

        unsafe { &mut *arena.chunk.add(self.byte_offset()).cast() }
    }

    pub(crate) fn cast<U>(self) -> ArenaRefMut<'a, U> {
        unsafe { core::mem::transmute::<ArenaRefMut<'a, T>, ArenaRefMut<'a, U>>(self) }
    }

    #[cfg(debug_assertions)]
    pub(crate) fn debug_verify_arena(&self, other_arena: &Arena<'a>, fn_name: &'static str) {
        // This only does anything in debug builds. In optimized builds, we don't do it.
        if (self.arena as *const _) != (other_arena as *const _) {
            panic!("{fn_name} was called passing a different arena from the one this ArenaRefMut was created with!");
        }
    }
}

impl<'a, T: Copy> ArenaRefMut<'a, T> {
    pub fn deref(&self, arena: &Arena<'a>) -> T {
        #[cfg(debug_assertions)]
        {
            self.debug_verify_arena(arena, "deref");
        }

        unsafe { ptr::read(arena.chunk.add(self.byte_offset()).cast()) }
    }
}

/////////////////////////////////////////////////////////////////////////////////////////////////////

/// A reference to something that has been allocated inside an Arena.
///
/// Importantly, it's stored as a byte offset into the arena's memory,
/// which means it can be serialized to/from disk and still work.
///
/// This also means that dereferencing it requires passing in the arena
/// where it was originally allocated. In debug builds, dereferencing will
/// do a check to make sure the arena being passed in is the same one that
/// was originally used to allocate the reference. (If not, it will panic.)
/// In release builds, this information is stored and nothing is checked at runtime.
pub struct ArenaRef<'a, T> {
    byte_offset_into_arena: u32,
    _marker: PhantomData<&'a T>,

    #[cfg(debug_assertions)]
    pub(crate) arena: &'a Arena<'a>,
}

impl<'a, T> Eq for ArenaRef<'a, T> {}

impl<'a, T> PartialEq for ArenaRef<'a, T> {
    fn eq(&self, other: &Self) -> bool {
        self.byte_offset_into_arena == other.byte_offset_into_arena
    }
}

impl<'a, T: Clone> Clone for ArenaRef<'a, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<'a, T: Copy> Copy for ArenaRef<'a, T> {}

impl<'a, T: fmt::Debug> fmt::Debug for ArenaRef<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let arg;

        #[cfg(debug_assertions)]
        {
            arg = self.as_ref(self.arena).fmt(f);
        }

        #[cfg(not(debug_assertions))]
        {
            arg = self.byte_offset_into_arena;
        }

        write!(f, "ArenaRef({:?})", arg)
    }
}

impl<'a, T> ArenaRef<'a, T> {
    pub(crate) const fn new_in(byte_offset_into_arena: u32, _arena: &Arena<'a>) -> Self {
        Self {
            byte_offset_into_arena,
            _marker: PhantomData,
            #[cfg(debug_assertions)]
            arena: _arena,
        }
    }

    pub(crate) const fn byte_offset(self) -> usize {
        self.byte_offset_into_arena as usize
    }

    pub(crate) const fn add_bytes(self, amount: u32) -> Self {
        Self {
            byte_offset_into_arena: self.byte_offset_into_arena + amount,
            _marker: PhantomData,

            #[cfg(debug_assertions)]
            arena: self.arena,
        }
    }

    pub fn as_ref(&self, arena: &Arena<'a>) -> &'a T {
        #[cfg(debug_assertions)]
        {
            self.debug_verify_arena(arena, "ArenaRef::deref");
        }

        unsafe { &*arena.chunk.add(self.byte_offset()).cast() }
    }

    pub(crate) fn cast<U>(self) -> ArenaRef<'a, U> {
        unsafe { core::mem::transmute::<ArenaRef<'a, T>, ArenaRef<'a, U>>(self) }
    }

    #[cfg(debug_assertions)]
    pub(crate) fn debug_verify_arena(&self, other_arena: &Arena<'a>, fn_name: &'static str) {
        // This only does anything in debug builds. In optimized builds, we don't do it.
        if (self.arena as *const _) != (other_arena as *const _) {
            panic!("{fn_name} was called passing a different arena from the one this ArenaRef was created with!");
        }
    }
}

impl<'a, T: Copy> ArenaRef<'a, T> {
    pub fn deref(&self, arena: &Arena<'a>) -> T {
        #[cfg(debug_assertions)]
        {
            self.debug_verify_arena(arena, "deref");
        }

        unsafe { ptr::read(arena.chunk.add(self.byte_offset()).cast()) }
    }
}
