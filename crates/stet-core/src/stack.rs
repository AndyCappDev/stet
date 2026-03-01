// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: AGPL-3.0-or-later

//! PostScript operand and execution stacks.

use crate::error::PsError;
use crate::object::PsObject;

/// A bounded stack of `PsObject` values.
pub struct Stack {
    data: Vec<PsObject>,
    max_size: usize,
}

impl Stack {
    pub fn new(max_size: usize) -> Self {
        Self {
            data: Vec::with_capacity(max_size.min(256)),
            max_size,
        }
    }

    /// Push an object, returning `StackOverflow` if full.
    #[inline]
    pub fn push(&mut self, obj: PsObject) -> Result<(), PsError> {
        if self.data.len() >= self.max_size {
            return Err(PsError::StackOverflow);
        }
        self.data.push(obj);
        Ok(())
    }

    /// Insert an object at a specific index from the bottom.
    ///
    /// Used by the ExecArray inner loop to insert a continuation cursor
    /// below items that an operator just pushed to e_stack.
    pub fn insert_at(&mut self, index: usize, obj: PsObject) -> Result<(), PsError> {
        if self.data.len() >= self.max_size {
            return Err(PsError::StackOverflow);
        }
        self.data.insert(index, obj);
        Ok(())
    }

    /// Pop the top object, returning `StackUnderflow` if empty.
    #[inline]
    pub fn pop(&mut self) -> Result<PsObject, PsError> {
        self.data.pop().ok_or(PsError::StackUnderflow)
    }

    /// Try to pop — returns `None` if empty (no error).
    #[inline]
    pub fn try_pop(&mut self) -> Option<PsObject> {
        self.data.pop()
    }

    /// Peek at an object relative to top. 0 = top, 1 = second from top, etc.
    #[inline]
    pub fn peek(&self, from_top: usize) -> Result<PsObject, PsError> {
        if from_top >= self.data.len() {
            return Err(PsError::StackUnderflow);
        }
        Ok(self.data[self.data.len() - 1 - from_top])
    }

    /// Mutable peek at an object relative to top.
    pub fn peek_mut(&mut self, from_top: usize) -> Result<&mut PsObject, PsError> {
        let len = self.data.len();
        if from_top >= len {
            return Err(PsError::StackUnderflow);
        }
        Ok(&mut self.data[len - 1 - from_top])
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn clear(&mut self) {
        self.data.clear();
    }

    /// Update the maximum stack size.
    pub fn set_max_size(&mut self, max_size: usize) {
        self.max_size = max_size;
    }

    pub fn as_slice(&self) -> &[PsObject] {
        &self.data
    }

    pub fn as_mut_slice(&mut self) -> &mut [PsObject] {
        &mut self.data
    }

    pub fn truncate(&mut self, len: usize) {
        self.data.truncate(len);
    }

    /// Swap the top two elements (for `exch`).
    pub fn swap_top_two(&mut self) -> Result<(), PsError> {
        let len = self.data.len();
        if len < 2 {
            return Err(PsError::StackUnderflow);
        }
        self.data.swap(len - 1, len - 2);
        Ok(())
    }

    /// Max capacity of this stack.
    pub fn max_size(&self) -> usize {
        self.max_size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_push_pop() {
        let mut s = Stack::new(10);
        s.push(PsObject::int(1)).unwrap();
        s.push(PsObject::int(2)).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s.pop().unwrap().as_i32(), Some(2));
        assert_eq!(s.pop().unwrap().as_i32(), Some(1));
        assert!(s.is_empty());
    }

    #[test]
    fn test_overflow() {
        let mut s = Stack::new(2);
        s.push(PsObject::int(1)).unwrap();
        s.push(PsObject::int(2)).unwrap();
        assert_eq!(s.push(PsObject::int(3)), Err(PsError::StackOverflow));
    }

    #[test]
    fn test_underflow() {
        let mut s = Stack::new(10);
        assert_eq!(s.pop(), Err(PsError::StackUnderflow));
    }

    #[test]
    fn test_peek() {
        let mut s = Stack::new(10);
        s.push(PsObject::int(10)).unwrap();
        s.push(PsObject::int(20)).unwrap();
        s.push(PsObject::int(30)).unwrap();
        assert_eq!(s.peek(0).unwrap().as_i32(), Some(30));
        assert_eq!(s.peek(1).unwrap().as_i32(), Some(20));
        assert_eq!(s.peek(2).unwrap().as_i32(), Some(10));
        assert_eq!(s.peek(3), Err(PsError::StackUnderflow));
    }

    #[test]
    fn test_swap_top_two() {
        let mut s = Stack::new(10);
        s.push(PsObject::int(1)).unwrap();
        s.push(PsObject::int(2)).unwrap();
        s.swap_top_two().unwrap();
        assert_eq!(s.peek(0).unwrap().as_i32(), Some(1));
        assert_eq!(s.peek(1).unwrap().as_i32(), Some(2));
    }
}
