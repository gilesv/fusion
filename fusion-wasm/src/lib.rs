use wasm_bindgen::prelude::*;
use wasm_bindgen::convert::FromWasmAbi;
use web_sys::{Element as HTMLElement, Text as HTMLText, Window, Document};
use js_sys::Reflect;
use std::cell::RefCell;
use std::rc::Rc;
use std::mem;

mod element;
mod fiber;
mod constants;
use element::{Element, ElementProps, Node};
use fiber::{Fiber, FiberCell, FiberEffect, FiberParentIterator};
use constants::{TEXT_ELEMENT, FIBER_ROOT, FIBER_FUNCTIONAL};

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace=console)]
    fn log(s: &str);
}
// Next let's define a macro that's like `println!`, only it works for
// `console.log`. Note that `println!` doesn't actually work on the wasm target
// because the standard library currently just eats all output. To get
// `println!`-like behavior in your app you'll likely want a macro like this.

#[macro_export]
macro_rules! console_log {
    // Note that this is using the `log` function imported above during
    // `bare_bones`
    ($($t:tt)*) => (unsafe { log(&format_args!($($t)*).to_string()); })
}

#[wasm_bindgen(inspectable)]
pub struct Context {
    wip_root: Option<FiberCell>,
    current_root: Option<FiberCell>,
    next_unit_of_work: Option<FiberCell>,
    wip_functional_fiber: Option<Fiber>,
    hook_index: usize,
    document: Document
}

#[wasm_bindgen]
impl Context {
    pub fn new() -> Self {
        let window: Window = web_sys::window().unwrap();
        let document: Document = window.document().unwrap();

        Context {
            wip_root: None,
            current_root: None,
            next_unit_of_work: None,
            wip_functional_fiber: None,
            hook_index: 0,
            document
        }
    }

    fn from_js_value(js_value: &JsValue) -> Result<Context, JsValue> {
        let ptr = unsafe { Reflect::get(&js_value, &JsValue::from_str("ptr"))? };
        let ptr_u32: u32 = ptr.as_f64().ok_or(JsValue::NULL)? as u32;
        let foo = unsafe { Context::from_abi(ptr_u32) };

        Ok(foo)
    }

    fn work_loop(&mut self, did_timeout: bool) -> Result<(), JsValue> {
        let mut no_next_unit_of_work = self.next_unit_of_work.is_none();

        loop {
            if did_timeout || no_next_unit_of_work {
                break;
            }

            self.next_unit_of_work = self.perform_unit_of_work();
            
            no_next_unit_of_work = self.next_unit_of_work.is_none();
        }

        if no_next_unit_of_work && self.wip_root.is_some() {
            self.commit_root()?;
        }

        Ok(())
    }

    fn perform_unit_of_work(&mut self) -> Option<FiberCell> {
        let wip_unit = self.next_unit_of_work.as_ref().unwrap();
        let mut fiber = wip_unit.borrow_mut();

        if fiber.is_functional_tree() {
            todo!();
        } else {
            // updateRegularTree
            if fiber.dom_node().is_none() {
                let dom_node = self.create_dom_node(&fiber);

                fiber.set_dom_node(Rc::new(RefCell::new(dom_node)));
            }

            self.reconcile_children(wip_unit, &mut fiber);
        }

        // If fiber has a child, make it the next unit of work
        if let Some(fiber_child) = &fiber.child() {
            return Some(Rc::clone(fiber_child));

        // ...or if it has a sibling, make it the next unit of work
        } else if let Some(fiber_sibling) = &fiber.sibling() {
            return Some(Rc::clone(fiber_sibling));

        // Otherwise look for the closest parent's sibling
        } else {
            // Drop the mutable borrow to avoid crashing when looping through the parents
            mem::drop(fiber);

            for parent in wip_unit.parents() {
                if let Some(parent_sibling) = &parent.borrow().sibling() {
                    return Some(Rc::clone(parent_sibling));
                }
            }
        }

        return None;
    }

    fn create_dom_node(&self, fiber: &Fiber) -> Node {
        let props = fiber.props().unwrap();

        if fiber.is_text_fiber() {
            let node: HTMLText = self.document.create_text_node(&props.node_value().unwrap());

            Node::Text(node)
        } else {
            let node = self.document.create_element(fiber.element_type()).unwrap();
            self.update_dom_node(&node, None, &props);

            Node::Element(node)
        }
    }

    fn update_dom_node(&self, dom_node: &HTMLElement, prev_props: Option<&ElementProps>, next_props: &ElementProps) {
        let prev_class_name = prev_props.and_then(|p| p.class_name());
        let next_class_name = next_props.class_name();

        match (prev_class_name, next_class_name) {
            (Some(prev), Some(next)) => {
                if *prev != *next {
                    dom_node.set_class_name(next);
                }
            },
            (None, Some(next)) => {
                dom_node.set_class_name(next);
            },
            (_, _) => {}
        }
    }

    fn update_dom_text(&self, text_node: &HTMLText, prev_props: Option<&ElementProps>, next_props: &ElementProps) {
        let prev_value = prev_props.and_then(|p| p.node_value());
        let next_value = next_props.node_value();

        match (prev_value, next_value) {
            (Some(prev), Some(next)) => {
                if *prev != *next {
                    text_node.set_node_value(Some(next));
                }
            },
            (None, Some(next)) => {
                text_node.set_node_value(Some(next));
            },
            (_, _) => {}
        }
    }

    fn reconcile_children(&self, wip_unit: &FiberCell, fiber: &mut Fiber) {
        let children = fiber.element_children().as_ref();
        let children_len = children.map_or(0, |children| { children.borrow().len() });

        let mut i = 0;
        let mut previous_sibling: Option<FiberCell> = None;
        let mut first_child_fiber: Option<FiberCell> = None;

        let mut old_child_fiber = if fiber.alternate().is_some() {
            let alternate = fiber.alternate().as_ref().unwrap().borrow();

            if let Some(child) = &alternate.child() {
                Some(Rc::clone(child))
            } else {
                None
            }
        } else {
            None
        };

        while i < children_len || old_child_fiber.is_some() {
            let child_element = &mut children.unwrap().borrow_mut()[i];

            let has_same_type = if let Some(old_child_cell) = &old_child_fiber {
                let old_child = old_child_cell.borrow();
                let old_child_type = old_child.element_type();
                let new_child_type = child_element.element_type();

                *old_child_type == *new_child_type
            } else {
                false
            };

            // Generate a new Fiber for the updated node
            let child_fiber = if has_same_type {
                // TODO: use same fiber instead of creating new ones
                let mut child_fiber = Fiber::new(&old_child_fiber.as_ref().unwrap().borrow().element_type());

                child_fiber.set_props(child_element.props_mut().take());

                let element_children = child_element.children_mut().take().map(|children| {
                    Rc::new(RefCell::new(children))
                });

                child_fiber.set_element_children(element_children);

                if let Some(old_child) = &old_child_fiber {
                    // relate to alternate
                    child_fiber.set_alternate(Rc::clone(&old_child));

                    // set existing dom node
                    if let Some(old_child_node) = old_child.borrow().dom_node() {
                        child_fiber.set_dom_node(Rc::clone(old_child_node));
                    }
                }

                // relate to parent (current fiber)
                child_fiber.set_parent(Rc::clone(wip_unit));

                // effect
                // TODO: set an effect only if props really changed
                child_fiber.set_effect(FiberEffect::Update);

                child_fiber
            } else {
                let mut child_fiber = Fiber::new(&child_element.element_type());

                child_fiber.set_props(child_element.props_mut().take());
                let element_children = child_element.children_mut().take().map(|children| {
                    Rc::new(RefCell::new(children))
                });
                child_fiber.set_element_children(element_children);

                // relate to parent (current fiber)
                child_fiber.set_parent(Rc::clone(wip_unit));

                // effect
                child_fiber.set_effect(FiberEffect::Placement);

                child_fiber
            };

            if old_child_fiber.is_some() && !has_same_type {
                old_child_fiber.as_ref().unwrap().borrow_mut().set_effect(FiberEffect::Deletion);
                // TODO: PUSH OLD CHILD FIBER TO DELETION ARRAY
            }

            if old_child_fiber.is_some() {
                let old_child_sibling = {
                    let old_child = old_child_fiber.as_ref().unwrap().borrow();
                    if let Some(sibling) = old_child.sibling() {
                        Some(Rc::clone(sibling))
                    } else {
                        None
                    }
                };

                old_child_fiber = old_child_sibling;
            }

            let child_fiber = Rc::new(RefCell::new(Box::new(child_fiber)));

            if i == 0 {
                first_child_fiber = Some(Rc::clone(&child_fiber));
            } else {
                if let Some(previous_sibling) = previous_sibling {
                    previous_sibling.borrow_mut().set_sibling(Rc::clone(&child_fiber));
                }
            }
            
            previous_sibling = Some(Rc::clone(&child_fiber));
            i += 1;

        }

        if let Some(child) = first_child_fiber {
            fiber.set_child(child);
        }
    }

    fn commit_root(&mut self) -> Result<(), JsValue> {
        if self.wip_root.is_some() {
            let wip_root_fiber = self.wip_root.as_ref().unwrap();

            self.commit_work(&wip_root_fiber.borrow().child())?;
            self.current_root = Some(Rc::clone(wip_root_fiber));
            self.wip_root = None;
        }

        Ok(())
    }

    fn commit_work(&self, fiber: &Option<FiberCell>) -> Result<(), JsValue> {
        if fiber.is_none() {
            return Ok(());
        }

        let fiber = fiber.as_ref().unwrap();

        match fiber.borrow().effect().as_ref() {
            Some(FiberEffect::Placement) => {
                let mut parent_dom_node = None;

                for parent in fiber.parents() {
                    let parent = parent.borrow();

                    if let Some(dom_node) = parent.dom_node() {
                        parent_dom_node = Some(Rc::clone(dom_node));
                        break;
                    }
                }

                self.commit_node_append(&fiber, parent_dom_node)?;
            },
            Some(FiberEffect::Update) => {
                let fiber = fiber.borrow();

                if let Some(dom_node) = fiber.dom_node() {
                    if let Some(alternate) = fiber.alternate() {
                        let alternate = alternate.borrow();
                        let prev_props = alternate.props();
                        let next_props = fiber.props().unwrap();
                        let node= &*dom_node.borrow();

                        match node {
                            Node::Element(node) => {
                                self.update_dom_node(
                                    &node,
                                    prev_props,
                                    next_props
                                );
                            },
                            Node::Text(text) => {
                                self.update_dom_text(
                                    text,
                                    prev_props,
                                    next_props
                                );
                            }
                        }
                    }
                }
            },
            Some(FiberEffect::Deletion) => {

            },
            None => {}
        }

        self.commit_work(&fiber.borrow().child())?;
        self.commit_work(&fiber.borrow().sibling())?;

        Ok(())
    }

    fn commit_node_append(&self, fiber: &FiberCell, parent_dom_node: Option<Rc<RefCell<Node>>>) -> Result<(), JsValue> {
        let has_dom_node = fiber.borrow().dom_node().is_some();
        let has_parent_node = parent_dom_node.is_some();

        if has_dom_node && has_parent_node {
            let fiber = fiber.borrow();
            let dom_node = fiber.dom_node().unwrap();
            let parent_node = parent_dom_node.unwrap();

            let dom_node = &*dom_node.borrow();
            let parent_node = &*parent_node.borrow();

            match (parent_node, dom_node) {
                // Append HTML element
                (Node::Element(parent), Node::Element(child)) => {
                    parent.append_child(&child)?;
                },

                // Append text node
                (Node::Element(parent), Node::Text(text)) => {
                    parent.append_child(&text)?;
                }
                _ => {}
            }
        }

        Ok(())
    }
}

#[wasm_bindgen]
pub fn get_context() -> Context {
    Context::new()
}

#[wasm_bindgen]
pub fn render(js_context: JsValue, js_element: JsValue, container: HTMLElement) -> Context {
    let element = Element::from_js_value(&js_element).unwrap();
    let mut context = Context::from_js_value(&js_context).unwrap();

    // Create the Root fiber
    let mut root = Fiber::new_root();
    
    // The root element will be the Root fiber's only child
    let mut children = Vec::with_capacity(1);
    children.push(element);
    root.set_element_children(Some(Rc::new(RefCell::new(children))));

    // Store the container HTML element
    root.set_dom_node(Rc::new(RefCell::new(Node::Element(container))));

    // Set the current root as the alternate root
    if let Some(current_root) = context.current_root.as_ref() {
        root.set_alternate(Rc::clone(current_root));
    }

    // Make it the Work in Progress Root and the Next Unit of Work
    let root = Rc::new(RefCell::new(Box::new(root)));
    context.wip_root = Some(Rc::clone(&root));
    context.next_unit_of_work = Some(Rc::clone(&root));

    context
}


#[wasm_bindgen]
pub fn work_loop(context_js: JsValue, did_timeout: bool) -> Context {
    console_error_panic_hook::set_once();
    let mut context: Context = Context::from_js_value(&context_js).unwrap();

    context.work_loop(did_timeout);

    context
}
